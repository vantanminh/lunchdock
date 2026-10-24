//! The dial-out gRPC session: greeting, command dispatch, and telemetry upload.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use rudder_core::{ByteStream, Error, NodeDriver, TerminalInput};
use rudder_node::ops::{self, Request, Streaming};
use rudder_proto::agent::{
    agent_message, server_message, AgentMessage, Command, CommandResult, Frame, Hello,
};
use rudder_proto::NodeAgentClient;
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

use crate::identity::Identity;
use crate::telemetry::Buffer;
use crate::update::{self, SelfUpdate};

const FLUSH_EVERY: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const STREAM_BUFFER: usize = 16;
/// How long to wait for the panel to finish reading a stream after its last frame.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

type Client = NodeAgentClient<Channel>;
type Outbox = mpsc::Sender<AgentMessage>;

pub struct Session {
    pub identity: Identity,
    pub driver: Arc<dyn NodeDriver>,
    pub buffer: Arc<Buffer>,
    pub self_update: Option<SelfUpdate>,
    /// Set once the panel welcomes a session; the update watchdog waits on it.
    pub connected: watch::Sender<bool>,
}

impl Session {
    /// Keep a session open, reconnecting with backoff, until the task is dropped.
    pub async fn run(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            match self.connect_once().await {
                Ok(()) => {
                    tracing::warn!("panel closed the agent session");
                    backoff = Duration::from_secs(1);
                }
                Err(SessionError::AfterWelcome(error)) => {
                    tracing::warn!(error = %format!("{error:#}"), "agent session lost");
                    backoff = Duration::from_secs(1);
                }
                Err(SessionError::Connect(error)) => {
                    tracing::warn!(
                        error = %format!("{error:#}"),
                        retry_in_secs = backoff.as_secs(),
                        "cannot reach the panel"
                    );
                }
            }
            self.buffer.requeue();
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    fn endpoint(&self) -> anyhow::Result<Endpoint> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let identity = &self.identity;
        let tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(&identity.ca_pem))
            .identity(tonic::transport::Identity::from_pem(
                &identity.cert_pem,
                &identity.key_pem,
            ))
            .domain_name(identity.meta.tls_name.clone());
        Ok(Endpoint::from_shared(identity.meta.grpc_url.clone())?
            .connect_timeout(Duration::from_secs(10))
            .tcp_keepalive(Some(Duration::from_secs(30)))
            .http2_keep_alive_interval(Duration::from_secs(30))
            .keep_alive_timeout(Duration::from_secs(20))
            .keep_alive_while_idle(true)
            .tls_config(tls)?)
    }

    async fn connect_once(self: &Arc<Self>) -> Result<(), SessionError> {
        let channel = self
            .endpoint()
            .map_err(SessionError::Connect)?
            .connect()
            .await
            .map_err(|e| SessionError::Connect(e.into()))?;
        let client = NodeAgentClient::new(channel);
        let (outbox, rx) = mpsc::channel(64);
        let hello = AgentMessage {
            body: Some(agent_message::Body::Hello(self.hello().await)),
        };
        let _ = outbox.send(hello).await;
        let mut incoming = client
            .clone()
            .session(ReceiverStream::new(rx))
            .await
            .map_err(|status| SessionError::Connect(status_error(status)))?
            .into_inner();
        match incoming.message().await {
            Ok(Some(message)) if matches!(message.body, Some(server_message::Body::Welcome(_))) => {
            }
            Ok(_) => {
                return Err(SessionError::Connect(anyhow::anyhow!(
                    "the panel did not welcome the agent"
                )))
            }
            Err(status) => return Err(SessionError::Connect(status_error(status))),
        }
        tracing::info!(panel = %self.identity.meta.grpc_url, "connected to the panel");
        self.connected.send_replace(true);

        let mut flush = tokio::time::interval(FLUSH_EVERY);
        loop {
            tokio::select! {
                message = incoming.message() => {
                    let message = message.map_err(|s| SessionError::AfterWelcome(status_error(s)))?;
                    let Some(message) = message else { return Ok(()) };
                    match message.body {
                        Some(server_message::Body::Command(command)) => {
                            let this = self.clone();
                            let client = client.clone();
                            let outbox = outbox.clone();
                            tokio::spawn(async move { this.dispatch(client, outbox, command).await });
                        }
                        Some(server_message::Body::Ack(ack)) => self.buffer.ack(ack.seq),
                        Some(server_message::Body::Welcome(_)) | None => {}
                    }
                }
                _ = flush.tick() => {
                    while let Some(batch) = self.buffer.next_batch() {
                        let message = AgentMessage { body: Some(agent_message::Body::Telemetry(batch)) };
                        if outbox.send(message).await.is_err() {
                            return Err(SessionError::AfterWelcome(anyhow::anyhow!("session closed")));
                        }
                    }
                }
            }
        }
    }

    async fn hello(&self) -> Hello {
        let docker = tokio::time::timeout(Duration::from_secs(5), self.driver.docker_info())
            .await
            .ok()
            .and_then(Result::ok);
        let host = tokio::time::timeout(Duration::from_secs(5), self.driver.host_stats())
            .await
            .ok()
            .and_then(Result::ok);
        Hello {
            agent_version: rudder_core::VERSION.to_owned(),
            hostname: crate::host::hostname(),
            docker_version: docker
                .as_ref()
                .map(|d| d.version.clone())
                .unwrap_or_default(),
            os_info: crate::host::os_info(),
            cpu_count: host
                .as_ref()
                .map(|h| h.cpu_count)
                .or_else(|| docker.as_ref().and_then(|d| u32::try_from(d.cpus).ok()))
                .unwrap_or_default(),
            memory_bytes: host
                .as_ref()
                .map(|h| h.memory_total_bytes)
                .or_else(|| {
                    docker
                        .as_ref()
                        .and_then(|d| u64::try_from(d.memory_bytes).ok())
                })
                .unwrap_or_default(),
        }
    }

    async fn dispatch(&self, client: Client, outbox: Outbox, command: Command) {
        let id = command.id;
        let request = match serde_json::from_slice::<Request>(&command.payload_json) {
            Ok(request) => request,
            Err(error) => {
                let error =
                    Error::Validation(format!("unsupported request {}: {error}", command.op));
                reply(&outbox, id, Err(error)).await;
                return;
            }
        };
        let driver = self.driver.as_ref();
        let stream_id = command.stream_id;
        match request {
            Request::AgentInfo => {
                let info = serde_json::json!({
                    "version": rudder_core::VERSION,
                    "target": update::target(),
                });
                return reply(&outbox, id, Ok(info)).await;
            }
            Request::UpdateAgent { version, sha256 } => {
                return self
                    .update_agent(client, outbox, id, &stream_id, &version, &sha256)
                    .await;
            }
            _ => {}
        }
        match request.streaming() {
            Streaming::None => reply(&outbox, id, ops::call(driver, request).await).await,
            Streaming::Output => {
                let data = match output_stream(driver, request).await {
                    Ok(data) => data,
                    Err(error) => return reply(&outbox, id, Err(error)).await,
                };
                reply(&outbox, id, Ok(Value::Null)).await;
                match open_channel(client, &stream_id).await {
                    Ok((frames, mut responses)) => {
                        let finished =
                            async move { while let Ok(Some(_)) = responses.message().await {} };
                        pump(frames, &stream_id, data, finished).await;
                    }
                    Err(error) => tracing::warn!(%error, "cannot open a stream to the panel"),
                }
            }
            Streaming::Input => {
                let (frames, responses) = match open_channel(client, &stream_id).await {
                    Ok(channel) => channel,
                    Err(error) => return reply(&outbox, id, Err(error)).await,
                };
                let input = input_bytes(responses);
                let outcome = match request {
                    Request::ExecWrite {
                        id: container,
                        spec,
                    } => driver
                        .exec_write(&container, spec, input)
                        .await
                        .and_then(json),
                    Request::ArchiveWrite { target } => {
                        driver.archive_write(&target, input).await.and_then(json)
                    }
                    Request::ExtractTree { path } => {
                        driver.extract_tree(&path, input).await.and_then(json)
                    }
                    Request::LoadImage => driver.load_image(input).await.and_then(json),
                    _ => Err(Error::Validation("not an upload".into())),
                };
                drop(frames);
                reply(&outbox, id, outcome).await;
            }
            Streaming::Both => {
                let Request::Terminal {
                    id: container,
                    cols,
                    rows,
                } = request
                else {
                    return reply(&outbox, id, Err(Error::Validation("not a terminal".into())))
                        .await;
                };
                let (frames, responses) = match open_channel(client, &stream_id).await {
                    Ok(channel) => channel,
                    Err(error) => return reply(&outbox, id, Err(error)).await,
                };
                let (input, input_done) = terminal_input(responses);
                let output = match driver.terminal(&container, cols, rows, input).await {
                    Ok(output) => output,
                    Err(error) => return reply(&outbox, id, Err(error)).await,
                };
                reply(&outbox, id, Ok(Value::Null)).await;
                let finished = async move {
                    let _ = input_done.await;
                    // Give the shell a moment to print its last output after input ends.
                    tokio::time::sleep(Duration::from_secs(5)).await;
                };
                pump(frames, &stream_id, output, finished).await;
            }
        }
    }
}

impl Session {
    async fn update_agent(
        &self,
        client: Client,
        outbox: Outbox,
        id: u64,
        stream_id: &str,
        version: &str,
        sha256: &str,
    ) {
        let (frames, responses) = match open_channel(client, stream_id).await {
            Ok(channel) => channel,
            Err(error) => return reply(&outbox, id, Err(error)).await,
        };
        let input = input_bytes(responses);
        let outcome = match &self.self_update {
            Some(target) => update::install(target, version, sha256, input).await,
            None => Err(Error::Validation(
                "this agent was started without self-update".into(),
            )),
        };
        drop(frames);
        let installed = outcome.is_ok();
        reply(&outbox, id, outcome.and_then(json)).await;
        if let (true, Some(target)) = (installed, &self.self_update) {
            // Let the reply reach the panel before the process exits.
            tokio::time::sleep(Duration::from_millis(500)).await;
            (target.restart)();
        }
    }
}

enum SessionError {
    Connect(anyhow::Error),
    AfterWelcome(anyhow::Error),
}

fn status_error(status: tonic::Status) -> anyhow::Error {
    anyhow::anyhow!("{}: {}", status.code(), status.message())
}

fn json<T: serde::Serialize>(value: T) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|e| Error::Node(e.to_string()))
}

async fn reply(outbox: &Outbox, id: u64, outcome: Result<Value, Error>) {
    let result = match outcome {
        Ok(value) => CommandResult {
            id,
            ok: true,
            error: String::new(),
            payload_json: serde_json::to_vec(&value).unwrap_or_default(),
        },
        Err(error) => {
            let validation = matches!(error, Error::Validation(_) | Error::InvalidName(_));
            CommandResult {
                id,
                ok: false,
                error: error.to_string(),
                payload_json: if validation {
                    b"validation".to_vec()
                } else {
                    Vec::new()
                },
            }
        }
    };
    let _ = outbox
        .send(AgentMessage {
            body: Some(agent_message::Body::Result(result)),
        })
        .await;
}

async fn output_stream(driver: &dyn NodeDriver, request: Request) -> Result<ByteStream, Error> {
    match request {
        Request::ExecRead { id, spec } => driver.exec_read(&id, spec).await,
        Request::ArchiveRead { source } => driver.archive_read(&source).await,
        Request::SaveImage { image } => driver.save_image(&image).await,
        Request::FollowLogs { id, since_unix } => {
            let lines = driver.follow_logs(&id, since_unix).await?;
            Ok(Box::pin(lines.map(|line| {
                line.and_then(|line| {
                    serde_json::to_vec(&line)
                        .map(Bytes::from)
                        .map_err(|e| Error::Node(e.to_string()))
                })
            })))
        }
        _ => Err(Error::Validation("not a download".into())),
    }
}

/// Open the `Channel` RPC for `stream_id`. The first frame only names the stream.
async fn open_channel(
    mut client: Client,
    stream_id: &str,
) -> Result<(mpsc::Sender<Frame>, tonic::Streaming<Frame>), Error> {
    let (frames, rx) = mpsc::channel(STREAM_BUFFER);
    let _ = frames
        .send(Frame {
            stream_id: stream_id.to_owned(),
            ..Frame::default()
        })
        .await;
    let responses = client
        .channel(ReceiverStream::new(rx))
        .await
        .map_err(|status| Error::Node(format!("open stream: {}", status.message())))?
        .into_inner();
    Ok((frames, responses))
}

/// Send `data` to the panel until it ends, fails, or `finished` resolves.
async fn pump(
    frames: mpsc::Sender<Frame>,
    stream_id: &str,
    mut data: ByteStream,
    finished: impl Future<Output = ()>,
) {
    let frame = |data: Vec<u8>, eof: bool, error: String| Frame {
        stream_id: stream_id.to_owned(),
        data,
        eof,
        error,
        ..Frame::default()
    };
    tokio::pin!(finished);
    loop {
        tokio::select! {
            item = data.next() => {
                let next = match item {
                    Some(Ok(bytes)) => frame(bytes.to_vec(), false, String::new()),
                    Some(Err(error)) => frame(Vec::new(), true, error.to_string()),
                    None => frame(Vec::new(), true, String::new()),
                };
                let last = next.eof;
                if frames.send(next).await.is_err() {
                    return;
                }
                if last {
                    break;
                }
            }
            () = &mut finished => return,
        }
    }
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, finished).await;
}

/// Upload bytes arriving from the panel, ending at its end-of-stream frame.
fn input_bytes(mut responses: tonic::Streaming<Frame>) -> ByteStream {
    let (tx, rx) = mpsc::channel(STREAM_BUFFER);
    tokio::spawn(async move {
        loop {
            let item = match responses.message().await {
                Ok(Some(frame)) if !frame.error.is_empty() => Err(Error::Node(frame.error)),
                Ok(Some(frame)) if frame.eof => break,
                Ok(Some(frame)) if frame.data.is_empty() => continue,
                Ok(Some(frame)) => Ok(Bytes::from(frame.data)),
                Ok(None) => Err(Error::Node("the panel ended the upload early".into())),
                Err(status) => Err(Error::Node(status.message().to_owned())),
            };
            let failed = item.is_err();
            if tx.send(item).await.is_err() || failed {
                break;
            }
        }
        drop(tx);
        // Read to the end so the panel's side closes cleanly.
        while let Ok(Some(_)) = responses.message().await {}
    });
    Box::pin(ReceiverStream::new(rx))
}

fn terminal_input(
    mut responses: tonic::Streaming<Frame>,
) -> (
    rudder_core::TerminalInputStream,
    tokio::sync::oneshot::Receiver<()>,
) {
    let (tx, rx) = mpsc::channel(STREAM_BUFFER);
    let (done, done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        while let Ok(Some(frame)) = responses.message().await {
            if frame.cols > 0 && frame.rows > 0 {
                let size = (u16::try_from(frame.cols), u16::try_from(frame.rows));
                if let (Ok(cols), Ok(rows)) = size {
                    if tx.send(TerminalInput::Resize { cols, rows }).await.is_err() {
                        break;
                    }
                }
            }
            if !frame.data.is_empty()
                && tx
                    .send(TerminalInput::Data(Bytes::from(frame.data)))
                    .await
                    .is_err()
            {
                break;
            }
            if frame.eof {
                break;
            }
        }
        drop(tx);
        let _ = done.send(());
    });
    (Box::pin(ReceiverStream::new(rx)), done_rx)
}
