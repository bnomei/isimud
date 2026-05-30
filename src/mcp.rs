//! MCP handlers for isimud using `rmcp` (PLAN.md task 7).
//!
//! Exposes the `isimud.speak`, `isimud.stop`, `isimud.list_voices`, and `isimud.status` tools
//! over streamable HTTP, and forwards speech-state notifications to connected peers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rmcp::{
    handler::server::wrapper::Parameters,
    model::{
        CustomNotification, CustomRequest, CustomResult, InitializeRequestParams, InitializeResult,
        ServerCapabilities, ServerInfo, ServerNotification,
    },
    tool, tool_handler, tool_router,
    transport::{
        streamable_http_server::session::local::LocalSessionManager, StreamableHttpServerConfig,
        StreamableHttpService,
    },
    ErrorData as McpError, Json, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

use crate::state::{JobOutcome, SpeechEvent, SpeechState};
use crate::voices::{SpeakRequest, VoiceResolveError};
use crate::worker::{SpeechEngine, VoiceCatalog};

/// Streamable HTTP service type for mounting on `/mcp` with axum/tower.
pub type IsimudMcpService = StreamableHttpService<IsimudMcp, LocalSessionManager>;

/// Maximum number of peers tracked for notification fan-out.
const MAX_MCP_PEERS: usize = 64;

/// Errors that can occur while initializing the MCP server.
#[derive(Debug, thiserror::Error)]
pub enum McpInitError {
    #[error("tokio runtime not available")]
    NoRuntime,
}

trait PeerHealth {
    fn transport_closed(&self) -> bool;
}

impl PeerHealth for rmcp::Peer<rmcp::RoleServer> {
    fn transport_closed(&self) -> bool {
        self.is_transport_closed()
    }
}

fn prune_closed_peers<P: PeerHealth>(peers: &mut Vec<P>) {
    peers.retain(|peer| !peer.transport_closed());
}

fn enforce_peer_cap<P>(peers: &mut Vec<P>, cap: usize) {
    if peers.len() > cap {
        let overflow = peers.len() - cap;
        peers.drain(0..overflow);
    }
}

/// isimud MCP server handler: bridges the [`SpeechEngine`] to MCP tools and forwards
/// [`SpeechEvent`]s as notifications to connected peers.
#[derive(Clone)]
pub struct IsimudMcp {
    engine: SpeechEngine,
    shutdown: Option<broadcast::Sender<()>>,
    peers: Arc<RwLock<Vec<rmcp::Peer<rmcp::RoleServer>>>>,
    forwarder_started: Arc<AtomicBool>,
}

impl IsimudMcp {
    /// Create a handler around the given engine.
    pub fn new(engine: SpeechEngine) -> Self {
        Self {
            engine,
            shutdown: None,
            peers: Arc::new(RwLock::new(Vec::new())),
            forwarder_started: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a handler that can trigger a graceful shutdown via the broadcast channel.
    pub fn new_with_shutdown(engine: SpeechEngine, shutdown: broadcast::Sender<()>) -> Self {
        Self {
            engine,
            shutdown: Some(shutdown),
            peers: Arc::new(RwLock::new(Vec::new())),
            forwarder_started: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Build a streamable-HTTP service wired for graceful shutdown, starting the forwarder.
    pub fn streamable_http_service_with_shutdown(
        engine: SpeechEngine,
        shutdown: broadcast::Sender<()>,
    ) -> Result<IsimudMcpService, McpInitError> {
        let handler = IsimudMcp::new_with_shutdown(engine, shutdown);
        handler.start_event_forwarder()?;
        Ok(StreamableHttpService::new(
            move || Ok(handler.clone()),
            Default::default(),
            StreamableHttpServerConfig::default(),
        ))
    }

    /// Trigger a graceful shutdown when an MCP client sends a recognized quit request.
    fn maybe_quit(&self, method: &str) {
        if !matches!(method, "isimud/quit" | "isimud/exit") {
            return;
        }
        if let Some(shutdown) = &self.shutdown {
            let _ = shutdown.send(());
        }
    }

    async fn register_peer(&self, peer: rmcp::Peer<rmcp::RoleServer>) {
        let mut peers = self.peers.write().await;
        prune_closed_peers(&mut peers);
        peers.push(peer);
        enforce_peer_cap(&mut peers, MAX_MCP_PEERS);
    }

    /// Spawn the task that forwards engine events to peers as MCP notifications.
    pub fn start_event_forwarder(&self) -> Result<(), McpInitError> {
        if self.forwarder_started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let mut subscription = self.engine.subscribe();
        let peers = self.peers.clone();
        let handle = tokio::runtime::Handle::try_current().map_err(|_| McpInitError::NoRuntime)?;

        handle.spawn(async move {
            loop {
                match subscription.recv().await {
                    Ok(event) => {
                        broadcast_notification(&peers, event_to_notification(&event)).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(())
    }
}

async fn broadcast_notification(
    peers: &Arc<RwLock<Vec<rmcp::Peer<rmcp::RoleServer>>>>,
    notification: ServerNotification,
) {
    let snapshot = peers.read().await.clone();
    let mut had_error = false;
    for peer in snapshot {
        if peer.send_notification(notification.clone()).await.is_err() {
            had_error = true;
        }
    }
    if had_error {
        let mut peers = peers.write().await;
        prune_closed_peers(&mut peers);
    }
}

fn event_to_notification(event: &SpeechEvent) -> ServerNotification {
    let payload = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
    ServerNotification::CustomNotification(CustomNotification::new(
        "isimud/speech_event",
        Some(payload),
    ))
}

fn map_resolve_error(error: VoiceResolveError) -> McpError {
    McpError::invalid_params(error.to_string(), None)
}

/// Parameters for `isimud.speak`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpeakParams {
    /// The text to speak aloud.
    pub text: String,
    /// Named voice from `[voices.*]`. Defaults to `[tts].default_voice`.
    #[serde(default)]
    pub voice: Option<String>,
    /// Speaking-rate multiplier (1.0 = normal). Overrides the voice/global rate.
    #[serde(default)]
    pub rate: Option<f32>,
    /// Block until the utterance finishes instead of returning immediately.
    #[serde(default)]
    pub wait: Option<bool>,
}

/// Result of `isimud.speak`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SpeakResult {
    /// The id assigned to the enqueued speech job.
    pub job_id: String,
    /// Queue depth observed after enqueueing.
    pub queue_depth: usize,
    /// Terminal outcome (`completed`/`failed`/`cancelled`), only when `wait` was true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Error detail when `outcome == "failed"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result of `isimud.stop`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct StopResult {
    /// The active job id that was cancelled, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled_job: Option<String>,
    /// Number of queued jobs that were discarded.
    pub cleared: usize,
}

/// Result of `isimud.status`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct StatusResult {
    /// `idle` or `speaking`.
    pub state: String,
    /// The active job id when speaking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// The active voice when speaking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// The active provider when speaking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Jobs waiting in the queue (excluding the active one).
    pub queue_depth: usize,
}

fn outcome_fields(outcome: JobOutcome) -> (Option<String>, Option<String>) {
    match outcome {
        JobOutcome::Completed => (Some("completed".to_string()), None),
        JobOutcome::Cancelled => (Some("cancelled".to_string()), None),
        JobOutcome::Failed { error } => (Some("failed".to_string()), Some(error)),
    }
}

#[tool_router]
impl IsimudMcp {
    #[tool(
        name = "isimud.speak",
        description = "Speak text aloud through the configured TTS provider. Returns a job id; \
                       set wait=true to block until the utterance finishes.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn speak(
        &self,
        Parameters(params): Parameters<SpeakParams>,
    ) -> Result<Json<SpeakResult>, McpError> {
        let request = SpeakRequest { text: params.text, voice: params.voice, rate: params.rate };

        if params.wait.unwrap_or(false) {
            let (job_id, receiver) =
                self.engine.enqueue_and_wait(request).map_err(map_resolve_error)?;
            let outcome = receiver.await.map_err(|_| {
                McpError::internal_error("speech job dropped before completion", None)
            })?;
            let (outcome, error) = outcome_fields(outcome);
            Ok(Json(SpeakResult {
                job_id: job_id.to_string(),
                queue_depth: self.engine.queue_depth(),
                outcome,
                error,
            }))
        } else {
            let job_id = self.engine.enqueue(request).map_err(map_resolve_error)?;
            Ok(Json(SpeakResult {
                job_id: job_id.to_string(),
                queue_depth: self.engine.queue_depth(),
                outcome: None,
                error: None,
            }))
        }
    }

    #[tool(
        name = "isimud.stop",
        description = "Stop the current utterance and clear any queued speech.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn stop(&self) -> Result<Json<StopResult>, McpError> {
        let (cancelled_job, cleared) = match self.engine.stop() {
            SpeechEvent::Stopped { cancelled_job, cleared } => {
                (cancelled_job.map(|id| id.to_string()), cleared)
            }
            _ => (None, 0),
        };
        Ok(Json(StopResult { cancelled_job, cleared }))
    }

    #[tool(
        name = "isimud.list_voices",
        description = "List configured named voices and the per-provider voices available.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_voices(&self) -> Result<Json<VoiceCatalog>, McpError> {
        Ok(Json(self.engine.list_voices().await))
    }

    #[tool(
        name = "isimud.status",
        description = "Report the current speech state and queue depth.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn status(&self) -> Result<Json<StatusResult>, McpError> {
        let snapshot = self.engine.status();
        let result = match snapshot.state {
            SpeechState::Idle => StatusResult {
                state: "idle".to_string(),
                job_id: None,
                voice: None,
                provider: None,
                queue_depth: snapshot.queue_depth,
            },
            SpeechState::Speaking { job_id, voice, provider } => StatusResult {
                state: "speaking".to_string(),
                job_id: Some(job_id.to_string()),
                voice: Some(voice),
                provider: Some(provider),
                queue_depth: snapshot.queue_depth,
            },
        };
        Ok(Json(result))
    }
}

#[tool_handler]
impl ServerHandler for IsimudMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "isimud speaks text aloud for AI agents. Use isimud.speak to enqueue speech \
             (optionally wait=true), isimud.stop to cancel, isimud.list_voices to discover \
             voices, and isimud.status to inspect current state.",
        )
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        if context.peer.peer_info().is_none() {
            context.peer.set_peer_info(request);
        }
        self.register_peer(context.peer.clone()).await;
        Ok(self.get_info())
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CustomResult, McpError> {
        self.maybe_quit(&request.method);
        Ok(CustomResult::new(serde_json::json!({ "ok": true })))
    }
}

#[cfg(test)]
mod tests {
    use super::{SpeakParams, SpeakResult, StatusResult, StopResult};
    use schemars::schema_for;

    #[test]
    fn tool_param_and_result_schemas_generate() {
        let _ = schema_for!(SpeakParams);
        let _ = schema_for!(SpeakResult);
        let _ = schema_for!(StatusResult);
        let _ = schema_for!(StopResult);
    }
}
