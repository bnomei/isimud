//! Speech state machine and event bus (PLAN.md task 3).
//!
//! Tracks whether isimud is idle or speaking a given job, so the menu bar can pulse and the
//! MCP server can report status / forward notifications.

use serde::Serialize;
use uuid::Uuid;

/// High-level speech state, surfaced to the tray and the MCP `status` tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SpeechState {
    /// Nothing is currently being spoken.
    #[default]
    Idle,
    /// A job is actively being synthesized / played.
    Speaking { job_id: Uuid, voice: String, provider: String },
}

impl SpeechState {
    /// Whether isimud is currently producing speech.
    pub fn is_speaking(&self) -> bool {
        matches!(self, SpeechState::Speaking { .. })
    }
}

/// A point-in-time view of the engine, returned by the `status` tool.
#[derive(Debug, Clone, Serialize)]
pub struct StatusSnapshot {
    pub state: SpeechState,
    pub queue_depth: usize,
}

/// Lifecycle events emitted by the speech worker and broadcast to subscribers
/// (the tray for pulse animation, the MCP server for peer notifications).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SpeechEvent {
    /// A new job was accepted onto the queue.
    Enqueued { job_id: Uuid, queue_depth: usize },
    /// A job began synthesizing / playing.
    Started {
        job_id: Uuid,
        voice: String,
        provider: String,
        text_preview: String,
        queue_depth: usize,
    },
    /// A job finished successfully.
    Finished { job_id: Uuid, queue_depth: usize },
    /// A job failed.
    Failed { job_id: Uuid, error: String, queue_depth: usize },
    /// Speech was stopped: the active job (if any) was cancelled and the queue cleared.
    Stopped { cancelled_job: Option<Uuid>, cleared: usize },
}

impl SpeechEvent {
    /// A short human-readable summary used for MCP log notifications.
    pub fn summary(&self) -> String {
        match self {
            SpeechEvent::Enqueued { job_id, queue_depth } => {
                format!("enqueued {job_id} (queue depth {queue_depth})")
            }
            SpeechEvent::Started { job_id, voice, provider, .. } => {
                format!("speaking {job_id} via {provider} voice '{voice}'")
            }
            SpeechEvent::Finished { job_id, .. } => format!("finished {job_id}"),
            SpeechEvent::Failed { job_id, error, .. } => format!("failed {job_id}: {error}"),
            SpeechEvent::Stopped { cancelled_job, cleared } => match cancelled_job {
                Some(job_id) => format!("stopped {job_id}, cleared {cleared} queued"),
                None => format!("stopped (idle), cleared {cleared} queued"),
            },
        }
    }
}

/// Outcome reported back to a caller that requested `wait = true`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum JobOutcome {
    Completed,
    Failed { error: String },
    Cancelled,
}
