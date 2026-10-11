//! The panel's side of the agent gRPC contract.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use futures_util::{Stream, StreamExt};
use rudder_core::{ContainerInfo, ContainerStats, HostStats, LogStreamKind};
use rudder_proto::agent::{
    agent_message, server_message, AgentMessage, Frame, Hello, LogRecord, ServerMessage,
    StatsSample, Telemetry, TelemetryAck, Welcome,
};
use rudder_proto::NodeAgent;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use super::pki;
use crate::driver::remote::RemoteDriver;
use crate::events::{self, Event};
use crate::logs::collector::{self, Target};
use crate::logs::NewLog;
use crate::monitor::Counters;
use crate::AppState;

const SESSION_BUFFER: usize = 64;

pub struct AgentService {
    state: AppState,
}

impl AgentService {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// The server a request's client certificate was issued to.
    async fn authenticate<T>(&self, request: &Request<T>) -> Result<Uuid, Status> {
        let certs = request
            .peer_certs()
            .ok_or_else(|| Status::unauthenticated("a client certificate is required"))?;
        let leaf = certs
            .first()
            .ok_or_else(|| Status::unauthenticated("a client certificate is required"))?;
        let fingerprint = pki::fingerprint(leaf.as_ref());
        sqlx::query_scalar!(
            "SELECT id FROM servers WHERE kind = 'remote' AND client_cert_fingerprint = $1",
            fingerprint
        )
        .fetch_optional(&self.state.pool)
        .await
        .map_err(|_| Status::unavailable("database unavailable"))?
        .ok_or_else(|| Status::permission_denied("this certificate is not enrolled"))
    }
}

type ServerStream = Pin<Box<dyn Stream<Item = Result<ServerMessage, Status>> + Send>>;
type FrameStream = Pin<Box<dyn Stream<Item = Result<Frame, Status>> + Send>>;

#[tonic::async_trait]
impl NodeAgent for AgentService {
    type SessionStream = ServerStream;
    type ChannelStream = FrameStream;

    async fn session(
        &self,
        request: Request<Streaming<AgentMessage>>,
    ) -> Result<Response<Self::SessionStream>, Status> {
        let server_id = self.authenticate(&request).await?;
        let mut incoming = request.into_inner();
        let hello = match incoming.next().await {
            Some(Ok(AgentMessage {
                body: Some(agent_message::Body::Hello(hello)),
            })) => hello,
            _ => return Err(Status::invalid_argument("the first message must be Hello")),
        };
        mark_online(&self.state, server_id, &hello)
            .await
            .map_err(|_| Status::unavailable("database unavailable"))?;

        let (tx, rx) = mpsc::channel(SESSION_BUFFER);
        let session = Arc::new(RemoteDriver::new(server_id, tx));
        let welcome = ServerMessage {
            body: Some(server_message::Body::Welcome(Welcome {
                server_id: server_id.to_string(),
                panel_version: rudder_core::VERSION.to_owned(),
            })),
        };
        session.push(welcome).await;
        self.state.agents.insert(session.clone());
        tracing::info!(%server_id, agent_version = %hello.agent_version, "agent connected");
        let publish_state = self.state.clone();
        tokio::spawn(async move {
            if let Err(error) = crate::publish::publish_server(&publish_state, server_id).await {
                tracing::warn!(%server_id, message = %error.public_message(), "routes were not applied after reconnect");
            }
        });

        let state = self.state.clone();
        tokio::spawn(async move {
            let mut ingest = Ingest::new(server_id);
            while let Some(message) = incoming.next().await {
                let Ok(message) = message else { break };
                match message.body {
                    Some(agent_message::Body::Result(result)) => session.resolve(result),
                    Some(agent_message::Body::Telemetry(batch)) => {
                        let seq = batch.seq;
                        if let Err(error) = ingest.batch(&state, batch).await {
                            tracing::warn!(%server_id, %error, "agent telemetry was not stored");
                            continue;
                        }
                        let ack = ServerMessage {
                            body: Some(server_message::Body::Ack(TelemetryAck { seq })),
                        };
                        session.push(ack).await;
                    }
                    Some(agent_message::Body::Hello(_)) | None => {}
                }
            }
            session.close();
            if state.agents.remove(server_id, session.session_id) {
                mark_offline(&state, server_id).await;
            }
            tracing::info!(%server_id, "agent disconnected");
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn channel(
        &self,
        request: Request<Streaming<Frame>>,
    ) -> Result<Response<Self::ChannelStream>, Status> {
        let server_id = self.authenticate(&request).await?;
        let mut incoming = request.into_inner();
        let first = incoming
            .next()
            .await
            .ok_or_else(|| Status::invalid_argument("empty channel"))??;
        let session = self
            .state
            .agents
            .session(server_id)
            .ok_or_else(|| Status::failed_precondition("no session for this agent"))?;
        let (tx, rx) = mpsc::channel(16);
        session.attach_channel(first, incoming, tx)?;
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

async fn mark_online(state: &AppState, server_id: Uuid, hello: &Hello) -> Result<(), sqlx::Error> {
    let was = sqlx::query_scalar!(
        r#"
        UPDATE servers s
        SET status = 'online', agent_version = $2, docker_version = NULLIF($3, ''),
            os_info = NULLIF($4, ''), hostname = NULLIF($5, ''), cpu_count = $6,
            memory_bytes = $7, last_heartbeat_at = now(), updated_at = now()
        FROM (SELECT status FROM servers WHERE id = $1) old
        WHERE s.id = $1
        RETURNING old.status
        "#,
        server_id,
        clip(&hello.agent_version, 64),
        clip(&hello.docker_version, 64),
        clip(&hello.os_info, 200),
        clip(&hello.hostname, 253),
        i32::try_from(hello.cpu_count).unwrap_or(i32::MAX),
        i64::try_from(hello.memory_bytes).unwrap_or(i64::MAX),
    )
    .fetch_optional(&state.pool)
    .await?;
    if was.as_deref() == Some("offline") {
        events::record(
            &state.pool,
            Event::info("server.online", "Server back online").server(server_id),
        )
        .await;
    }
    Ok(())
}

async fn mark_offline(state: &AppState, server_id: Uuid) {
    let updated = sqlx::query!(
        "UPDATE servers SET status = 'offline', updated_at = now() WHERE id = $1 AND status = 'online'",
        server_id
    )
    .execute(&state.pool)
    .await;
    if matches!(updated, Ok(result) if result.rows_affected() > 0) {
        events::record(
            &state.pool,
            Event::warning("server.offline", "Agent disconnected").server(server_id),
        )
        .await;
    }
}

fn clip(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn unix_ms(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(Utc::now)
}

/// Per-session state for turning agent telemetry into stored rows.
struct Ingest {
    server_id: Uuid,
    targets: HashMap<String, Option<Target>>,
    counters: Counters,
}

impl Ingest {
    fn new(server_id: Uuid) -> Self {
        Self {
            server_id,
            targets: HashMap::new(),
            counters: Counters::default(),
        }
    }

    async fn batch(&mut self, state: &AppState, batch: Telemetry) -> anyhow::Result<()> {
        let mut stats = batch.stats;
        stats.sort_by_key(|sample| sample.sampled_at_unix_ms);
        for sample in stats {
            self.stats(state, sample).await?;
        }
        let sender = state.logs.sender();
        for record in batch.logs {
            if let Some(entry) = self.log(state, record).await? {
                if sender.send(entry).await.is_err() {
                    break;
                }
            }
        }
        sqlx::query!(
            r#"
            UPDATE servers
            SET last_heartbeat_at = now(), status = 'online',
                telemetry_dropped = telemetry_dropped + $2
            WHERE id = $1
            "#,
            self.server_id,
            i64::try_from(batch.dropped).unwrap_or(i64::MAX)
        )
        .execute(&state.pool)
        .await?;
        Ok(())
    }

    async fn stats(&mut self, state: &AppState, sample: StatsSample) -> anyhow::Result<()> {
        let host: HostStats = serde_json::from_slice(&sample.host_json)?;
        let containers: Vec<ContainerStats> = if sample.containers_json.is_empty() {
            Vec::new()
        } else {
            serde_json::from_slice(&sample.containers_json)?
        };
        let at = unix_ms(sample.sampled_at_unix_ms).min(Utc::now());
        crate::monitor::record_stats(
            &state.pool,
            self.server_id,
            at,
            &host,
            &containers,
            &mut self.counters,
        )
        .await?;
        Ok(())
    }

    async fn log(&mut self, state: &AppState, record: LogRecord) -> anyhow::Result<Option<NewLog>> {
        let logged_at = unix_ms(record.logged_at_unix_ms).min(Utc::now());
        if record.container_id.is_empty() {
            if record.text.trim().is_empty() {
                return Ok(None);
            }
            let parsed = rudder_core::parse_line(&record.text);
            return Ok(Some(NewLog {
                logged_at,
                server_id: Some(self.server_id),
                project_id: None,
                service_id: None,
                source: "agent".into(),
                level: parsed.level.into(),
                message: parsed.message,
                http_status: None,
                fields: serde_json::Value::Object(parsed.fields),
            }));
        }
        let container = ContainerInfo {
            id: record.container_id.clone(),
            name: record.container_name,
            image: String::new(),
            labels: serde_json::from_str(&record.labels_json).unwrap_or_default(),
        };
        if !self.targets.contains_key(&container.id) {
            let target = collector::resolve(state, &container).await?;
            self.targets.insert(container.id.clone(), target);
        }
        let Some(Some(target)) = self.targets.get(&container.id) else {
            return Ok(None);
        };
        let stream = if record.stderr {
            LogStreamKind::Stderr
        } else {
            LogStreamKind::Stdout
        };
        Ok(collector::to_entry(
            self.server_id,
            &container,
            target,
            stream,
            logged_at,
            record.text,
        ))
    }
}
