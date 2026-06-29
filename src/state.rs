//! Speech state machine, status snapshots, and lifecycle events.
//!
//! Tracks idle vs speaking for the menu-bar pulse and MCP `status` tool. The worker broadcasts
//! [`SpeechEvent`]s to subscribers (tray animation, MCP peer notifications).

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
    /// Current idle or speaking state.
    pub state: SpeechState,
    /// Jobs waiting behind the active utterance.
    pub queue_depth: usize,
    /// Whether the speech subsystem needs user attention (worker panic, etc.).
    pub degraded: bool,
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
    /// The speech subsystem entered a degraded health state and needs user attention.
    Degraded { reason: String },
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
            SpeechEvent::Degraded { reason } => format!("degraded: {reason}"),
        }
    }
}

/// Terminal outcome reported to a caller that used `wait = true` or `enqueue_and_wait`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum JobOutcome {
    /// Synthesis and playback finished without error.
    Completed,
    /// The job failed during provider selection, synthesis, or playback.
    Failed { error: String },
    /// The job was cancelled by `stop` or superseded shutdown.
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job_id() -> Uuid {
        Uuid::nil()
    }

    #[test]
    fn is_speaking_reflects_variant() {
        assert!(!SpeechState::Idle.is_speaking());
        let speaking = SpeechState::Speaking {
            job_id: job_id(),
            voice: "alloy".to_string(),
            provider: "openai".to_string(),
        };
        assert!(speaking.is_speaking());
    }

    #[test]
    fn summary_covers_every_event_variant() {
        let id = job_id();
        assert_eq!(
            SpeechEvent::Enqueued { job_id: id, queue_depth: 2 }.summary(),
            format!("enqueued {id} (queue depth 2)")
        );
        assert_eq!(
            SpeechEvent::Started {
                job_id: id,
                voice: "alloy".to_string(),
                provider: "openai".to_string(),
                text_preview: "hi".to_string(),
                queue_depth: 0,
            }
            .summary(),
            format!("speaking {id} via openai voice 'alloy'")
        );
        assert_eq!(
            SpeechEvent::Finished { job_id: id, queue_depth: 0 }.summary(),
            format!("finished {id}")
        );
        assert_eq!(
            SpeechEvent::Failed { job_id: id, error: "boom".to_string(), queue_depth: 0 }.summary(),
            format!("failed {id}: boom")
        );
        assert_eq!(
            SpeechEvent::Stopped { cancelled_job: Some(id), cleared: 3 }.summary(),
            format!("stopped {id}, cleared 3 queued")
        );
        assert_eq!(
            SpeechEvent::Stopped { cancelled_job: None, cleared: 1 }.summary(),
            "stopped (idle), cleared 1 queued"
        );
        assert_eq!(
            SpeechEvent::Degraded { reason: "worker exited".to_string() }.summary(),
            "degraded: worker exited"
        );
    }
}
