//! Serialized speech worker and job queue (PLAN.md task 4).
//!
//! A single background task drains a queue so utterances never overlap. Enqueue is
//! fire-and-forget and returns a job id; callers may optionally wait until a job completes.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, oneshot, Notify};
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::AppConfig;
use crate::playback::{self, PlaybackError};
use crate::providers::{CancelToken, ProviderError, ProviderRegistry, SpeechOutput, VoiceInfo};
use crate::state::{JobOutcome, SpeechEvent, SpeechState, StatusSnapshot};
use crate::voices::{resolve_speech, ResolvedSpeech, SpeakRequest, VoiceResolveError};
use crate::{config::ProviderKind, TARGET_SPEECH};

/// Capacity of the broadcast channel carrying [`SpeechEvent`]s to subscribers.
const EVENT_CHANNEL_CAPACITY: usize = 256;
/// Maximum length of the text preview embedded in events.
const PREVIEW_LEN: usize = 60;

/// A queued speech job, after voice resolution.
struct Job {
    id: Uuid,
    resolved: ResolvedSpeech,
    preview: String,
    waiter: Option<oneshot::Sender<JobOutcome>>,
}

/// The job currently being spoken, with its cancellation handle.
struct CurrentJob {
    id: Uuid,
    cancel: CancelToken,
}

struct EngineInner {
    config: Arc<AppConfig>,
    registry: ProviderRegistry,
    queue: Mutex<VecDeque<Job>>,
    notify: Notify,
    current: Mutex<Option<CurrentJob>>,
    state: Mutex<SpeechState>,
    events: broadcast::Sender<SpeechEvent>,
    shutdown: AtomicBool,
}

/// The serialized speech engine: owns the provider registry, queue, and worker task.
#[derive(Clone)]
pub struct SpeechEngine {
    inner: Arc<EngineInner>,
}

impl SpeechEngine {
    /// Build an engine from configuration (does not start the worker task yet).
    pub fn new(config: Arc<AppConfig>) -> Self {
        let registry = ProviderRegistry::from_config(&config);
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(EngineInner {
                config,
                registry,
                queue: Mutex::new(VecDeque::new()),
                notify: Notify::new(),
                current: Mutex::new(None),
                state: Mutex::new(SpeechState::Idle),
                events,
                shutdown: AtomicBool::new(false),
            }),
        }
    }

    /// Spawn the background worker task. Returns its [`tokio::task::JoinHandle`].
    pub fn start(&self) -> tokio::task::JoinHandle<()> {
        let inner = self.inner.clone();
        tokio::spawn(async move { worker_loop(inner).await })
    }

    /// The active application configuration.
    pub fn config(&self) -> &Arc<AppConfig> {
        &self.inner.config
    }

    /// Subscribe to the speech-event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<SpeechEvent> {
        self.inner.events.subscribe()
    }

    /// Current queue depth (jobs waiting, excluding the active one).
    pub fn queue_depth(&self) -> usize {
        self.inner.queue.lock().expect("queue mutex poisoned").len()
    }

    /// A point-in-time status snapshot.
    pub fn status(&self) -> StatusSnapshot {
        let state = self.inner.state.lock().expect("state mutex poisoned").clone();
        StatusSnapshot { state, queue_depth: self.queue_depth() }
    }

    /// Enqueue a speech job, returning its id. Fails fast if the voice cannot be resolved.
    pub fn enqueue(&self, request: SpeakRequest) -> Result<Uuid, VoiceResolveError> {
        self.enqueue_inner(request, None)
    }

    /// Enqueue a job and obtain a receiver that resolves when the job completes/fails/cancels.
    pub fn enqueue_and_wait(
        &self,
        request: SpeakRequest,
    ) -> Result<(Uuid, oneshot::Receiver<JobOutcome>), VoiceResolveError> {
        let (tx, rx) = oneshot::channel();
        let id = self.enqueue_inner(request, Some(tx))?;
        Ok((id, rx))
    }

    fn enqueue_inner(
        &self,
        request: SpeakRequest,
        waiter: Option<oneshot::Sender<JobOutcome>>,
    ) -> Result<Uuid, VoiceResolveError> {
        let resolved = resolve_speech(&self.inner.config, &request)?;
        let id = Uuid::new_v4();
        let preview = preview_text(&resolved.text);

        let depth = {
            let mut queue = self.inner.queue.lock().expect("queue mutex poisoned");
            queue.push_back(Job { id, resolved, preview, waiter });
            queue.len()
        };
        self.inner.notify.notify_one();
        let _ = self.inner.events.send(SpeechEvent::Enqueued { job_id: id, queue_depth: depth });
        Ok(id)
    }

    /// Cancel the active job (if any) and clear the queue. Returns the emitted event.
    pub fn stop(&self) -> SpeechEvent {
        let cancelled_job = {
            let current = self.inner.current.lock().expect("current mutex poisoned");
            current.as_ref().map(|job| {
                job.cancel.cancel();
                job.id
            })
        };

        let cleared: Vec<Job> = {
            let mut queue = self.inner.queue.lock().expect("queue mutex poisoned");
            queue.drain(..).collect()
        };
        let cleared_count = cleared.len();
        for job in cleared {
            if let Some(waiter) = job.waiter {
                let _ = waiter.send(JobOutcome::Cancelled);
            }
        }

        let event = SpeechEvent::Stopped { cancelled_job, cleared: cleared_count };
        let _ = self.inner.events.send(event.clone());
        event
    }

    /// Enumerate configured named voices plus best-effort per-provider voices.
    pub async fn list_voices(&self) -> VoiceCatalog {
        let named = self
            .inner
            .config
            .voices
            .iter()
            .map(|(name, voice)| NamedVoiceEntry {
                name: name.clone(),
                provider: voice.provider,
                voice: voice.voice.clone(),
            })
            .collect();
        let providers = self.inner.registry.list_all_voices().await;
        VoiceCatalog { named, providers }
    }
}

/// The named voices configured in `[voices.*]`.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct NamedVoiceEntry {
    pub name: String,
    pub provider: ProviderKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
}

/// Combined view returned by `list_voices`.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct VoiceCatalog {
    pub named: Vec<NamedVoiceEntry>,
    pub providers: Vec<(ProviderKind, Vec<VoiceInfo>)>,
}

fn preview_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= PREVIEW_LEN {
        return trimmed.to_string();
    }
    let prefix: String = trimmed.chars().take(PREVIEW_LEN).collect();
    format!("{prefix}…")
}

impl SpeechEngine {
    /// Signal the worker to stop after the current job, cancelling any active playback.
    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        if let Some(job) = self.inner.current.lock().expect("current mutex poisoned").as_ref() {
            job.cancel.cancel();
        }
        self.inner.notify.notify_one();
    }
}

/// Internal run outcome distinguishing cancellation from genuine failure.
enum RunError {
    Cancelled,
    Failed(String),
}

async fn worker_loop(inner: Arc<EngineInner>) {
    info!(target: TARGET_SPEECH, "speech worker started");
    loop {
        if inner.shutdown.load(Ordering::SeqCst) {
            break;
        }
        let job = {
            let mut queue = inner.queue.lock().expect("queue mutex poisoned");
            queue.pop_front()
        };
        match job {
            Some(job) => process_job(&inner, job).await,
            None => inner.notify.notified().await,
        }
    }
    info!(target: TARGET_SPEECH, "speech worker stopped");
}

async fn process_job(inner: &Arc<EngineInner>, job: Job) {
    let Job { id, resolved, preview, waiter } = job;

    let cancel = CancelToken::new();
    {
        let mut current = inner.current.lock().expect("current mutex poisoned");
        *current = Some(CurrentJob { id, cancel: cancel.clone() });
    }
    let remaining = inner.queue.lock().expect("queue mutex poisoned").len();

    let selection = inner.registry.select(&resolved, &inner.config.tts.providers).await;

    let outcome = match selection {
        Some((provider, eff)) => {
            let provider_name = eff.provider.to_string();
            {
                let mut state = inner.state.lock().expect("state mutex poisoned");
                *state = SpeechState::Speaking {
                    job_id: id,
                    voice: eff.voice_name.clone(),
                    provider: provider_name.clone(),
                };
            }
            let _ = inner.events.send(SpeechEvent::Started {
                job_id: id,
                voice: eff.voice_name.clone(),
                provider: provider_name,
                text_preview: preview,
                queue_depth: remaining,
            });

            match run_job(provider, &eff, &cancel).await {
                Ok(()) => {
                    let _ = inner.events.send(SpeechEvent::Finished {
                        job_id: id,
                        queue_depth: inner.queue.lock().expect("queue mutex poisoned").len(),
                    });
                    JobOutcome::Completed
                }
                Err(RunError::Cancelled) => JobOutcome::Cancelled,
                Err(RunError::Failed(error)) => {
                    warn!(target: TARGET_SPEECH, job_id = %id, %error, "speech job failed");
                    let _ = inner.events.send(SpeechEvent::Failed {
                        job_id: id,
                        error: error.clone(),
                        queue_depth: inner.queue.lock().expect("queue mutex poisoned").len(),
                    });
                    JobOutcome::Failed { error }
                }
            }
        }
        None => {
            let error = format!(
                "no available provider for voice '{}' (provider {})",
                resolved.voice_name, resolved.provider
            );
            warn!(target: TARGET_SPEECH, %error, "speech job has no available provider");
            let _ = inner.events.send(SpeechEvent::Failed {
                job_id: id,
                error: error.clone(),
                queue_depth: inner.queue.lock().expect("queue mutex poisoned").len(),
            });
            JobOutcome::Failed { error }
        }
    };

    {
        let mut current = inner.current.lock().expect("current mutex poisoned");
        *current = None;
    }
    {
        let mut state = inner.state.lock().expect("state mutex poisoned");
        *state = SpeechState::Idle;
    }

    if let Some(waiter) = waiter {
        let _ = waiter.send(outcome);
    }
}

async fn run_job(
    provider: Arc<dyn crate::providers::TtsProvider>,
    eff: &ResolvedSpeech,
    cancel: &CancelToken,
) -> Result<(), RunError> {
    match provider.synthesize(eff, cancel).await {
        Ok(SpeechOutput::PlayedInline) => Ok(()),
        Ok(SpeechOutput::Audio(bytes)) => match playback::play_audio(bytes, cancel.clone()).await {
            Ok(()) => Ok(()),
            Err(PlaybackError::Cancelled) => Err(RunError::Cancelled),
            Err(error) => Err(RunError::Failed(error.to_string())),
        },
        Err(ProviderError::Cancelled) => Err(RunError::Cancelled),
        Err(error) => Err(RunError::Failed(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::SpeechEngine;
    use crate::config::{AppConfig, ProviderKind, VoiceConfig};
    use crate::state::SpeechEvent;
    use crate::voices::SpeakRequest;
    use std::sync::Arc;

    fn engine() -> SpeechEngine {
        let mut config = AppConfig::default();
        config.voices.insert(
            "default".to_string(),
            VoiceConfig {
                provider: ProviderKind::OpenAi,
                voice: Some("alloy".to_string()),
                language: None,
                rate: None,
                pitch: None,
                volume: None,
            },
        );
        SpeechEngine::new(Arc::new(config))
    }

    #[test]
    fn enqueue_increments_queue_and_stop_clears_it() {
        let engine = engine();
        assert_eq!(engine.queue_depth(), 0);

        engine
            .enqueue(SpeakRequest { text: "hello".to_string(), voice: None, rate: None })
            .expect("enqueue should succeed");
        assert_eq!(engine.queue_depth(), 1);

        match engine.stop() {
            SpeechEvent::Stopped { cleared, .. } => assert_eq!(cleared, 1),
            other => panic!("expected Stopped, got {other:?}"),
        }
        assert_eq!(engine.queue_depth(), 0);
    }

    #[test]
    fn enqueue_rejects_unknown_voice() {
        let engine = engine();
        let result = engine.enqueue(SpeakRequest {
            text: "hi".to_string(),
            voice: Some("ghost".to_string()),
            rate: None,
        });
        assert!(result.is_err());
        assert_eq!(engine.queue_depth(), 0);
    }
}
