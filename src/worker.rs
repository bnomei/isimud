//! Serialized speech worker and job queue (PLAN.md task 4).
//!
//! A single background task drains a queue so utterances never overlap. Enqueue is
//! fire-and-forget and returns a job id; callers may optionally wait until a job completes.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use arc_swap::ArcSwap;
use tokio::sync::{broadcast, oneshot, Notify};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::AppConfig;
use crate::playback::{self, PlaybackError};
use crate::providers::{CancelToken, ProviderError, ProviderRegistry, SpeechOutput, VoiceInfo};
use crate::state::{JobOutcome, SpeechEvent, SpeechState, StatusSnapshot};
use crate::voices::{resolve_speech, ResolvedSpeech, SpeakRequest, VoiceResolveError};
use crate::{config::ProviderKind, TARGET_SPEECH};
use thiserror::Error;

/// Capacity of the broadcast channel carrying [`SpeechEvent`]s to subscribers.
const EVENT_CHANNEL_CAPACITY: usize = 256;
/// Maximum length of the text preview embedded in events.
const PREVIEW_LEN: usize = 60;

type JobWaiter = Arc<Mutex<Option<oneshot::Sender<JobOutcome>>>>;

/// A queued speech job, after voice resolution.
struct Job {
    id: Uuid,
    resolved: ResolvedSpeech,
    preview: String,
    waiter: Option<JobWaiter>,
}

/// The job currently being spoken, with its cancellation handle.
struct CurrentJob {
    id: Uuid,
    cancel: CancelToken,
}

struct EngineInner {
    config: ArcSwap<AppConfig>,
    registry: ArcSwap<ProviderRegistry>,
    queue: Mutex<VecDeque<Job>>,
    notify: Notify,
    current: Mutex<Option<CurrentJob>>,
    state: Mutex<SpeechState>,
    events: broadcast::Sender<SpeechEvent>,
    shutdown: AtomicBool,
    degraded: AtomicBool,
}

impl EngineInner {
    fn mark_degraded(&self, reason: impl Into<String>) {
        let reason = reason.into();
        self.degraded.store(true, Ordering::SeqCst);
        let _ = self.events.send(SpeechEvent::Degraded { reason });
    }
}

fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>, name: &'static str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        warn!(target: TARGET_SPEECH, lock = name, "speech mutex poisoned; recovering");
        poisoned.into_inner()
    })
}

fn send_waiter(waiter: Option<JobWaiter>, outcome: JobOutcome) {
    if let Some(waiter) = waiter {
        if let Some(sender) = lock_or_recover(&waiter, "waiter").take() {
            let _ = sender.send(outcome);
        }
    }
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
        Self::with_registry(config, registry)
    }

    /// Build an engine from configuration and an injected provider registry.
    pub fn with_registry(config: Arc<AppConfig>, registry: ProviderRegistry) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(EngineInner {
                config: ArcSwap::new(config),
                registry: ArcSwap::new(Arc::new(registry)),
                queue: Mutex::new(VecDeque::new()),
                notify: Notify::new(),
                current: Mutex::new(None),
                state: Mutex::new(SpeechState::Idle),
                events,
                shutdown: AtomicBool::new(false),
                degraded: AtomicBool::new(false),
            }),
        }
    }

    /// Spawn the background worker task. Returns its [`tokio::task::JoinHandle`].
    pub fn start(&self) -> tokio::task::JoinHandle<()> {
        let inner = self.inner.clone();
        tokio::spawn(async move { worker_loop(inner).await })
    }

    /// The active application configuration (a snapshot taken at call time).
    pub fn config(&self) -> Arc<AppConfig> {
        self.inner.config.load_full()
    }

    /// Atomically swap in a new configuration, rebuilding the provider registry so that
    /// changed voices, rates, and provider credentials take effect on the next job.
    pub fn reload_config(&self, new_config: Arc<AppConfig>) {
        let registry = ProviderRegistry::from_config(&new_config);
        self.inner.registry.store(Arc::new(registry));
        self.inner.config.store(new_config);
    }

    /// Subscribe to the speech-event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<SpeechEvent> {
        self.inner.events.subscribe()
    }

    /// Current queue depth (jobs waiting, excluding the active one).
    pub fn queue_depth(&self) -> usize {
        lock_or_recover(&self.inner.queue, "queue").len()
    }

    /// A point-in-time status snapshot.
    pub fn status(&self) -> StatusSnapshot {
        let state = lock_or_recover(&self.inner.state, "state").clone();
        StatusSnapshot {
            state,
            queue_depth: self.queue_depth(),
            degraded: self.inner.degraded.load(Ordering::SeqCst),
        }
    }

    /// Configured wait timeout, in seconds; 0 means wait forever.
    pub fn wait_timeout_secs(&self) -> u64 {
        self.inner.config.load().tts.wait_timeout_secs
    }

    /// Mark the engine as degraded and broadcast a health event.
    pub fn mark_degraded(&self, reason: impl Into<String>) {
        self.inner.mark_degraded(reason);
    }

    /// Whether the worker has been asked to shut down normally.
    pub fn is_shutdown(&self) -> bool {
        self.inner.shutdown.load(Ordering::SeqCst)
    }

    /// Enqueue a speech job, returning its id. Fails fast if the voice cannot be resolved.
    pub fn enqueue(&self, request: SpeakRequest) -> Result<Uuid, EnqueueError> {
        self.enqueue_inner(request, None)
    }

    /// Enqueue a job and obtain a receiver that resolves when the job completes/fails/cancels.
    pub fn enqueue_and_wait(
        &self,
        request: SpeakRequest,
    ) -> Result<(Uuid, oneshot::Receiver<JobOutcome>), EnqueueError> {
        let (tx, rx) = oneshot::channel();
        let id = self.enqueue_inner(request, Some(Arc::new(Mutex::new(Some(tx)))))?;
        Ok((id, rx))
    }

    fn enqueue_inner(
        &self,
        request: SpeakRequest,
        waiter: Option<JobWaiter>,
    ) -> Result<Uuid, EnqueueError> {
        let config = self.inner.config.load();
        let resolved = resolve_speech(&config, &request)?;
        let id = Uuid::new_v4();
        let preview = preview_text(&resolved.text);

        let depth = {
            let mut queue = lock_or_recover(&self.inner.queue, "queue");
            let capacity = config.tts.max_queue_depth;
            // A capacity of 0 intentionally disables backpressure for users who prefer unbounded
            // fire-and-forget queuing.
            if capacity > 0 && queue.len() >= capacity {
                return Err(EnqueueError::QueueFull { depth: queue.len(), capacity });
            }
            queue.push_back(Job { id, resolved, preview, waiter });
            queue.len()
        };
        self.inner.notify.notify_one();
        let _ = self.inner.events.send(SpeechEvent::Enqueued { job_id: id, queue_depth: depth });
        Ok(id)
    }

    /// Cancel the active job (if any) and clear the queue. Returns the emitted event.
    pub fn stop(&self) -> SpeechEvent {
        // Hold the queue lock across the current-job check (lock order: queue→current, the
        // same order worker_loop uses). worker_loop moves a job from the queue into `current`
        // atomically under the queue lock, so taking the queue lock here makes stop() atomic
        // against that move: the just-dequeued job is always observed in exactly one of
        // `queue` (drained) or `current` (cancelled), never missed in the gap between them.
        let (cancelled_job, cleared) = {
            let mut queue = lock_or_recover(&self.inner.queue, "queue");
            let cancelled_job = {
                let current = lock_or_recover(&self.inner.current, "current");
                current.as_ref().map(|job| {
                    job.cancel.cancel();
                    job.id
                })
            };
            let cleared: Vec<Job> = queue.drain(..).collect();
            (cancelled_job, cleared)
        };
        let cleared_count = cleared.len();
        for job in cleared {
            send_waiter(job.waiter, JobOutcome::Cancelled);
        }

        let event = SpeechEvent::Stopped { cancelled_job, cleared: cleared_count };
        let _ = self.inner.events.send(event.clone());
        event
    }

    /// Enumerate configured named voices plus best-effort per-provider voices.
    pub async fn list_voices(&self) -> VoiceCatalog {
        let config = self.inner.config.load_full();
        let named = config
            .voices
            .iter()
            .map(|(name, voice)| NamedVoiceEntry {
                name: name.clone(),
                provider: voice.provider,
                voice: voice.voice.clone(),
            })
            .collect();
        let registry = self.inner.registry.load_full();
        let providers = registry.list_all_voices().await;
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

/// Errors raised while accepting a speech job into the queue.
#[derive(Debug, Error)]
pub enum EnqueueError {
    #[error(transparent)]
    Resolve(#[from] VoiceResolveError),
    #[error("speech queue is full ({depth}/{capacity})")]
    QueueFull { depth: usize, capacity: usize },
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
        // Same atomicity as stop(): take the queue lock (queue→current order) so the worker
        // cannot be mid-move of a just-dequeued job from the queue into `current` while we
        // read `current`, which would otherwise let that job escape cancellation.
        {
            let _queue = lock_or_recover(&self.inner.queue, "queue");
            if let Some(job) = lock_or_recover(&self.inner.current, "current").as_ref() {
                job.cancel.cancel();
            }
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
        let next = {
            let mut queue = lock_or_recover(&inner.queue, "queue");
            match queue.pop_front() {
                Some(job) => {
                    // Register the job as "current" with its cancel handle while still holding
                    // the queue lock. This closes the dequeue→register race: a stop() can no
                    // longer slip between pop_front() and registration to find an empty current
                    // and empty queue, then cancel nothing while the utterance plays in full.
                    let cancel = CancelToken::new();
                    {
                        let mut current = lock_or_recover(&inner.current, "current");
                        *current = Some(CurrentJob { id: job.id, cancel: cancel.clone() });
                    }
                    Some((job, cancel))
                }
                None => None,
            }
        };
        match next {
            Some((job, cancel)) => {
                let job_id = job.id;
                let waiter = job.waiter.clone();
                let handle = tokio::spawn(process_job(inner.clone(), job, cancel));
                if let Err(error) = handle.await {
                    let reason = format!("speech job {job_id} panicked or was aborted: {error}");
                    error!(target: TARGET_SPEECH, job_id = %job_id, %error, "speech job task failed");
                    handle_panicked_job(&inner, job_id, waiter, reason);
                }
            }
            None => inner.notify.notified().await,
        }
    }
    info!(target: TARGET_SPEECH, "speech worker stopped");
}

fn handle_panicked_job(
    inner: &Arc<EngineInner>,
    job_id: Uuid,
    waiter: Option<JobWaiter>,
    reason: String,
) {
    {
        let mut current = lock_or_recover(&inner.current, "current");
        if current.as_ref().is_some_and(|current| current.id == job_id) {
            *current = None;
        }
    }
    {
        let mut state = lock_or_recover(&inner.state, "state");
        *state = SpeechState::Idle;
    }
    inner.mark_degraded(reason.clone());
    let _ = inner.events.send(SpeechEvent::Failed {
        job_id,
        error: reason.clone(),
        queue_depth: lock_or_recover(&inner.queue, "queue").len(),
    });
    send_waiter(waiter, JobOutcome::Failed { error: reason });
}

async fn process_job(inner: Arc<EngineInner>, job: Job, cancel: CancelToken) {
    let Job { id, resolved, preview, waiter } = job;

    // `cancel` and the `inner.current` registration are established by `worker_loop` under the
    // queue lock before this task is spawned, so a racing stop() always observes this job.
    let remaining = lock_or_recover(&inner.queue, "queue").len();

    let registry = inner.registry.load_full();
    let config = inner.config.load_full();
    let selection = registry.select(&resolved, &config.tts.providers).await;

    let outcome = match selection {
        Some((provider, eff)) => {
            let provider_name = eff.provider.to_string();
            {
                let mut state = lock_or_recover(&inner.state, "state");
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
                        queue_depth: lock_or_recover(&inner.queue, "queue").len(),
                    });
                    JobOutcome::Completed
                }
                Err(RunError::Cancelled) => JobOutcome::Cancelled,
                Err(RunError::Failed(error)) => {
                    warn!(target: TARGET_SPEECH, job_id = %id, %error, "speech job failed");
                    let _ = inner.events.send(SpeechEvent::Failed {
                        job_id: id,
                        error: error.clone(),
                        queue_depth: lock_or_recover(&inner.queue, "queue").len(),
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
                queue_depth: lock_or_recover(&inner.queue, "queue").len(),
            });
            JobOutcome::Failed { error }
        }
    };

    {
        let mut current = lock_or_recover(&inner.current, "current");
        *current = None;
    }
    {
        let mut state = lock_or_recover(&inner.state, "state");
        *state = SpeechState::Idle;
    }

    send_waiter(waiter, outcome);
}

async fn run_job(
    provider: Arc<dyn crate::providers::TtsProvider>,
    eff: &ResolvedSpeech,
    cancel: &CancelToken,
) -> Result<(), RunError> {
    match provider.synthesize(eff, cancel).await {
        Ok(SpeechOutput::PlayedInline) => Ok(()),
        Ok(SpeechOutput::Audio(bytes)) => {
            match playback::play_audio(bytes, cancel.clone(), eff.volume).await {
                Ok(()) => Ok(()),
                Err(PlaybackError::Cancelled) => Err(RunError::Cancelled),
                Err(error) => Err(RunError::Failed(error.to_string())),
            }
        }
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
