//! [`NodeDriver`] for a remote server, forwarded over its agent's gRPC session.
//!
//! Unary calls are a `Command` answered by a `CommandResult`. Streaming calls
//! register a slot under a random stream id first; the agent then opens a
//! `Channel` RPC naming that id, and [`RemoteDriver::attach_channel`] connects it
//! to the slot. Every stream gets its own HTTP/2 stream and flow control, so a
//! large backup never delays commands or telemetry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use rudder_core::{
    ArchiveSource, ByteStream, CachePurge, CachePurgeResult, CacheStats, ComposeAction,
    ComposeOptions, ContainerInfo, ContainerSpec, ContainerStats, DockerInfo, Error, ExecOutput,
    ExecSpec, HealthState, HostStats, LogLine, LogLineStream, LogSink, NodeDriver, RegistryAuth,
    ReplaceContainer, ReplaceReport, TerminalInput, TerminalInputStream,
};
use rudder_node::ops::{self, Request};
use rudder_proto::agent::{server_message, Command, CommandResult, Frame, ServerMessage};
use serde::de::DeserializeOwned;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;
use uuid::Uuid;

/// An unclaimed stream slot is dropped after this long.
const CLAIM_TIMEOUT: Duration = Duration::from_secs(60);
const STREAM_BUFFER: usize = 16;

pub type ServerSender = mpsc::Sender<Result<ServerMessage, Status>>;
pub type FrameSender = mpsc::Sender<Result<Frame, Status>>;

enum Slot {
    /// Agent to panel.
    Output(mpsc::Sender<Result<Bytes, Error>>),
    /// Panel to agent.
    Input(ByteStream),
    Terminal {
        output: mpsc::Sender<Result<Bytes, Error>>,
        input: TerminalInputStream,
    },
}

pub struct RemoteDriver {
    pub server_id: Uuid,
    pub session_id: Uuid,
    tx: Mutex<Option<ServerSender>>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<CommandResult>>>,
    slots: Mutex<HashMap<String, Slot>>,
}

impl RemoteDriver {
    pub fn new(server_id: Uuid, tx: ServerSender) -> Self {
        Self {
            server_id,
            session_id: Uuid::new_v4(),
            tx: Mutex::new(Some(tx)),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Deliver a `CommandResult` from the agent to its waiting caller.
    pub fn resolve(&self, result: CommandResult) {
        let waiter = self
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&result.id));
        if let Some(waiter) = waiter {
            let _ = waiter.send(result);
        }
    }

    /// Fail every waiting call and stream; the session has ended.
    pub fn close(&self) {
        if let Ok(mut tx) = self.tx.lock() {
            tx.take();
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
        if let Ok(mut slots) = self.slots.lock() {
            slots.clear();
        }
    }

    pub fn is_open(&self) -> bool {
        self.sender().is_some_and(|tx| !tx.is_closed())
    }

    fn sender(&self) -> Option<ServerSender> {
        self.tx.lock().ok().and_then(|tx| tx.clone())
    }

    /// Send a message on the session outside a command, such as a telemetry ack.
    pub async fn push(&self, message: ServerMessage) -> bool {
        match self.sender() {
            Some(tx) => tx.send(Ok(message)).await.is_ok(),
            None => false,
        }
    }

    fn take_slot(&self, stream_id: &str) -> Option<Slot> {
        self.slots.lock().ok()?.remove(stream_id)
    }

    fn register(self: &std::sync::Arc<Self>, slot: Slot) -> String {
        let stream_id = Uuid::new_v4().simple().to_string();
        if let Ok(mut slots) = self.slots.lock() {
            slots.insert(stream_id.clone(), slot);
        }
        let this = std::sync::Arc::downgrade(self);
        let id = stream_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CLAIM_TIMEOUT).await;
            if let Some(driver) = this.upgrade() {
                driver.take_slot(&id);
            }
        });
        stream_id
    }

    async fn send(&self, request: &Request, stream_id: &str) -> Result<CommandResult, Error> {
        let payload = serde_json::to_vec(request).map_err(|e| Error::Node(e.to_string()))?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (waiter, result) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, waiter);
        }
        let message = ServerMessage {
            body: Some(server_message::Body::Command(Command {
                id,
                op: request.name().to_owned(),
                payload_json: payload,
                stream_id: stream_id.to_owned(),
            })),
        };
        if !self.push(message).await {
            self.forget(id);
            return Err(disconnected());
        }
        let timeout = Duration::from_secs(request.timeout_secs());
        match tokio::time::timeout(timeout, result).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(disconnected()),
            Err(_) => {
                self.forget(id);
                Err(Error::Node(format!(
                    "agent did not answer {} within {}s",
                    request.name(),
                    timeout.as_secs()
                )))
            }
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
    }

    async fn call<T: DeserializeOwned>(&self, request: Request) -> Result<T, Error> {
        let value = self.call_value(request, "").await?;
        serde_json::from_value(value).map_err(|e| Error::Node(format!("agent reply: {e}")))
    }

    async fn call_value(
        &self,
        request: Request,
        stream_id: &str,
    ) -> Result<serde_json::Value, Error> {
        let result = self.send(&request, stream_id).await?;
        if !result.ok {
            return Err(remote_error(&result));
        }
        if result.payload_json.is_empty() {
            return Ok(serde_json::Value::Null);
        }
        serde_json::from_slice(&result.payload_json)
            .map_err(|e| Error::Node(format!("agent reply: {e}")))
    }

    async fn output_stream(
        self: &std::sync::Arc<Self>,
        request: Request,
    ) -> Result<ReceiverStream<Result<Bytes, Error>>, Error> {
        let (sink, stream) = mpsc::channel(STREAM_BUFFER);
        let stream_id = self.register(Slot::Output(sink));
        if let Err(error) = self.call_value(request, &stream_id).await {
            self.take_slot(&stream_id);
            return Err(error);
        }
        Ok(ReceiverStream::new(stream))
    }

    async fn input_stream<T: DeserializeOwned>(
        self: &std::sync::Arc<Self>,
        request: Request,
        input: ByteStream,
    ) -> Result<T, Error> {
        let stream_id = self.register(Slot::Input(input));
        let result = self.call_value(request, &stream_id).await;
        self.take_slot(&stream_id);
        serde_json::from_value(result?).map_err(|e| Error::Node(format!("agent reply: {e}")))
    }

    /// Connect a `Channel` RPC from the agent to its slot. The returned frames go
    /// back to the agent.
    pub fn attach_channel(
        &self,
        first: Frame,
        mut incoming: tonic::Streaming<Frame>,
        outgoing: FrameSender,
    ) -> Result<(), Status> {
        let stream_id = first.stream_id.clone();
        let slot = self
            .take_slot(&stream_id)
            .ok_or_else(|| Status::not_found("unknown or expired stream"))?;
        match slot {
            Slot::Output(sink) => {
                tokio::spawn(async move {
                    if forward_frame(&sink, first).await {
                        while let Some(frame) = incoming.next().await {
                            let frame = match frame {
                                Ok(frame) => frame,
                                Err(status) => {
                                    let _ =
                                        sink.send(Err(Error::Node(status.message().into()))).await;
                                    break;
                                }
                            };
                            if !forward_frame(&sink, frame).await {
                                break;
                            }
                        }
                    }
                    drop(outgoing);
                });
            }
            Slot::Input(mut input) => {
                tokio::spawn(async move {
                    while let Some(item) = input.next().await {
                        let frame = match item {
                            Ok(data) => Frame {
                                stream_id: stream_id.clone(),
                                data: data.to_vec(),
                                ..Frame::default()
                            },
                            Err(error) => Frame {
                                stream_id: stream_id.clone(),
                                eof: true,
                                error: error.to_string(),
                                ..Frame::default()
                            },
                        };
                        let failed = frame.eof;
                        if outgoing.send(Ok(frame)).await.is_err() || failed {
                            return;
                        }
                    }
                    let _ = outgoing
                        .send(Ok(Frame {
                            stream_id,
                            eof: true,
                            ..Frame::default()
                        }))
                        .await;
                    // Keep reading so the agent's half closes cleanly.
                    while incoming.next().await.is_some() {}
                });
            }
            Slot::Terminal { output, mut input } => {
                let id = stream_id.clone();
                tokio::spawn(async move {
                    while let Some(item) = input.next().await {
                        let frame = match item {
                            TerminalInput::Data(data) => Frame {
                                stream_id: id.clone(),
                                data: data.to_vec(),
                                ..Frame::default()
                            },
                            TerminalInput::Resize { cols, rows } => Frame {
                                stream_id: id.clone(),
                                cols: u32::from(cols),
                                rows: u32::from(rows),
                                ..Frame::default()
                            },
                        };
                        if outgoing.send(Ok(frame)).await.is_err() {
                            return;
                        }
                    }
                    let _ = outgoing
                        .send(Ok(Frame {
                            stream_id: id,
                            eof: true,
                            ..Frame::default()
                        }))
                        .await;
                });
                tokio::spawn(async move {
                    if forward_frame(&output, first).await {
                        while let Some(Ok(frame)) = incoming.next().await {
                            if !forward_frame(&output, frame).await {
                                break;
                            }
                        }
                    }
                });
            }
        }
        Ok(())
    }
}

/// Push one agent frame into a byte sink. Returns false once the stream is over.
async fn forward_frame(sink: &mpsc::Sender<Result<Bytes, Error>>, frame: Frame) -> bool {
    if !frame.data.is_empty() && sink.send(Ok(Bytes::from(frame.data))).await.is_err() {
        return false;
    }
    if !frame.error.is_empty() {
        let _ = sink.send(Err(Error::Node(frame.error))).await;
        return false;
    }
    !frame.eof
}

fn disconnected() -> Error {
    Error::Node("the agent for this server disconnected".into())
}

fn remote_error(result: &CommandResult) -> Error {
    if result.payload_json == b"validation" {
        Error::Validation(result.error.clone())
    } else {
        Error::Node(result.error.clone())
    }
}

/// `NodeDriver` is implemented on the `Arc` so streaming calls can register slots
/// that outlive the call.
pub struct RemoteHandle(pub std::sync::Arc<RemoteDriver>);

#[async_trait]
impl NodeDriver for RemoteHandle {
    async fn docker_info(&self) -> Result<DockerInfo, Error> {
        self.0.call(Request::DockerInfo).await
    }

    async fn pull_image(&self, image: &str, auth: Option<RegistryAuth>) -> Result<(), Error> {
        self.0
            .call(Request::PullImage {
                image: image.into(),
                auth,
            })
            .await
    }

    async fn create_container(&self, spec: &ContainerSpec) -> Result<String, Error> {
        self.0
            .call(Request::CreateContainer { spec: spec.clone() })
            .await
    }

    async fn start_container(&self, id: &str) -> Result<(), Error> {
        self.0.call(Request::StartContainer { id: id.into() }).await
    }

    async fn stop_container(&self, id: &str, timeout_secs: i64) -> Result<(), Error> {
        self.0
            .call(Request::StopContainer {
                id: id.into(),
                timeout_secs,
            })
            .await
    }

    async fn restart_container(&self, id: &str) -> Result<(), Error> {
        self.0
            .call(Request::RestartContainer { id: id.into() })
            .await
    }

    async fn remove_container(&self, id: &str, force: bool) -> Result<(), Error> {
        self.0
            .call(Request::RemoveContainer {
                id: id.into(),
                force,
            })
            .await
    }

    async fn container_running(&self, id: &str) -> Result<bool, Error> {
        self.0
            .call(Request::ContainerRunning { id: id.into() })
            .await
    }

    async fn container_logs(&self, id: &str, tail: u32) -> Result<String, Error> {
        self.0
            .call(Request::ContainerLogs {
                id: id.into(),
                tail,
            })
            .await
    }

    async fn exec(&self, id: &str, command: Vec<String>) -> Result<ExecOutput, Error> {
        self.0
            .call(Request::Exec {
                id: id.into(),
                command,
            })
            .await
    }

    async fn write_file(&self, relative_path: &str, bytes: &[u8]) -> Result<(), Error> {
        self.0
            .call(Request::WriteFile {
                path: relative_path.into(),
                data: bytes.to_vec(),
            })
            .await
    }

    async fn compose(
        &self,
        project: &str,
        file: &str,
        action: ComposeAction,
    ) -> Result<String, Error> {
        self.0
            .call(Request::Compose {
                project: project.into(),
                file: file.into(),
                action,
                logins: Vec::new(),
            })
            .await
    }

    /// The agent runs the command and answers when it ends, so the sink gets the
    /// whole output at once.
    async fn compose_run(
        &self,
        project: &str,
        file: &str,
        action: ComposeAction,
        options: &ComposeOptions,
        sink: Option<&LogSink>,
    ) -> Result<String, Error> {
        let output: String = self
            .0
            .call(Request::Compose {
                project: project.into(),
                file: file.into(),
                action,
                logins: options.logins.clone(),
            })
            .await?;
        if let Some(sink) = sink {
            sink(&output);
        }
        Ok(output)
    }

    async fn docker_health(&self, id: &str) -> Result<HealthState, Error> {
        self.0.call(Request::DockerHealth { id: id.into() }).await
    }

    async fn http_ready(&self, id: &str, port: u16, path: &str) -> Result<bool, Error> {
        self.0
            .call(Request::HttpReady {
                id: id.into(),
                port,
                path: path.into(),
            })
            .await
    }

    async fn tcp_ready(&self, id: &str, port: u16) -> Result<bool, Error> {
        self.0
            .call(Request::TcpReady {
                id: id.into(),
                port,
            })
            .await
    }

    async fn image_digest(&self, image: &str) -> Result<String, Error> {
        self.0
            .call(Request::ImageDigest {
                image: image.into(),
            })
            .await
    }

    async fn exec_read(&self, id: &str, spec: ExecSpec) -> Result<ByteStream, Error> {
        let stream = self
            .0
            .output_stream(Request::ExecRead {
                id: id.into(),
                spec,
            })
            .await?;
        Ok(Box::pin(stream))
    }

    async fn exec_write(
        &self,
        id: &str,
        spec: ExecSpec,
        input: ByteStream,
    ) -> Result<ExecOutput, Error> {
        self.0
            .input_stream(
                Request::ExecWrite {
                    id: id.into(),
                    spec,
                },
                input,
            )
            .await
    }

    async fn archive_read(&self, source: &ArchiveSource) -> Result<ByteStream, Error> {
        source.validate()?;
        let stream = self
            .0
            .output_stream(Request::ArchiveRead {
                source: source.clone(),
            })
            .await?;
        Ok(Box::pin(stream))
    }

    async fn archive_write(&self, target: &ArchiveSource, input: ByteStream) -> Result<(), Error> {
        target.validate()?;
        self.0
            .input_stream(
                Request::ArchiveWrite {
                    target: target.clone(),
                },
                input,
            )
            .await
    }

    async fn save_image(&self, image: &str) -> Result<ByteStream, Error> {
        let stream = self
            .0
            .output_stream(Request::SaveImage {
                image: image.into(),
            })
            .await?;
        Ok(Box::pin(stream))
    }

    async fn load_image(&self, input: ByteStream) -> Result<(), Error> {
        self.0.input_stream(Request::LoadImage, input).await
    }

    async fn remove_volume(&self, name: &str) -> Result<(), Error> {
        self.0
            .call(Request::RemoveVolume { name: name.into() })
            .await
    }

    async fn container_stats(&self) -> Result<Vec<ContainerStats>, Error> {
        self.0.call(Request::ContainerStats).await
    }

    async fn prune(&self) -> Result<u64, Error> {
        self.0.call(Request::Prune).await
    }

    async fn read_file(&self, relative_path: &str) -> Result<Vec<u8>, Error> {
        let value = self
            .0
            .call_value(
                Request::ReadFile {
                    path: relative_path.into(),
                },
                "",
            )
            .await?;
        ops::decode_read_file(value)
    }

    async fn cache_purge(&self, request: &CachePurge) -> Result<CachePurgeResult, Error> {
        self.0
            .call(Request::CachePurge {
                request: request.clone(),
            })
            .await
    }

    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, Error> {
        self.0.call(Request::ListContainers).await
    }

    async fn follow_logs(&self, id: &str, since_unix: i64) -> Result<LogLineStream, Error> {
        let stream = self
            .0
            .output_stream(Request::FollowLogs {
                id: id.into(),
                since_unix,
            })
            .await?;
        let lines = stream.map(|item| {
            item.and_then(|bytes| {
                serde_json::from_slice::<LogLine>(&bytes)
                    .map_err(|e| Error::Node(format!("agent log frame: {e}")))
            })
        });
        Ok(Box::pin(lines))
    }

    async fn terminal(
        &self,
        id: &str,
        cols: u16,
        rows: u16,
        input: TerminalInputStream,
    ) -> Result<ByteStream, Error> {
        let (output, stream) = mpsc::channel(STREAM_BUFFER);
        let stream_id = self.0.register(Slot::Terminal { output, input });
        if let Err(error) = self
            .0
            .call_value(
                Request::Terminal {
                    id: id.into(),
                    cols,
                    rows,
                },
                &stream_id,
            )
            .await
        {
            self.0.take_slot(&stream_id);
            return Err(error);
        }
        Ok(Box::pin(ReceiverStream::new(stream)))
    }

    async fn host_stats(&self) -> Result<HostStats, Error> {
        self.0.call(Request::HostStats).await
    }

    async fn cache_stats(&self) -> Result<CacheStats, Error> {
        self.0.call(Request::CacheStats).await
    }

    async fn container_image(&self, name: &str) -> Result<Option<String>, Error> {
        self.0
            .call(Request::ContainerImage { name: name.into() })
            .await
    }

    async fn replace_container(&self, request: &ReplaceContainer) -> Result<ReplaceReport, Error> {
        self.0
            .call(Request::ReplaceContainer {
                request: request.clone(),
            })
            .await
    }
}

/// What a connected agent reports about itself.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AgentInfo {
    pub version: String,
    pub target: String,
}

impl RemoteHandle {
    pub async fn agent_info(&self) -> Result<AgentInfo, Error> {
        self.0.call(Request::AgentInfo).await
    }

    /// Upload a new agent binary. The agent restarts after it replies.
    pub async fn update_agent(
        &self,
        version: &str,
        sha256: &str,
        binary: ByteStream,
    ) -> Result<(), Error> {
        self.0
            .input_stream(
                Request::UpdateAgent {
                    version: version.into(),
                    sha256: sha256.into(),
                },
                binary,
            )
            .await
    }
}
