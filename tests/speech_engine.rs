//! Speech-engine integration tests with an injected fake provider.
//!
//! Exercises enqueue/wait, fire-and-forget, stop/cancel, queue-full backpressure, and
//! degraded recovery when a job panics — without synthesizing or playing audio.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use isimud::config::{AppConfig, ProviderKind, VoiceConfig};
use isimud::providers::{
    CancelToken, ProviderError, ProviderRegistry, SpeechOutput, TtsProvider, VoiceInfo,
};
use isimud::state::JobOutcome;
use isimud::voices::{ResolvedSpeech, SpeakRequest};
use isimud::worker::{EnqueueError, SpeechEngine};

/// A recorded synthesis call captured by the fake provider.
#[derive(Debug, Clone, PartialEq)]
struct Recorded {
    text: String,
    voice_id: Option<String>,
    rate: f32,
    volume: f32,
}

/// A no-audio provider that records calls and optionally cancels/panics/sleeps.
struct FakeProvider {
    calls: Arc<Mutex<Vec<Recorded>>>,
    panic_once: Arc<Mutex<bool>>,
    sleep: Duration,
}

impl FakeProvider {
    fn new(calls: Arc<Mutex<Vec<Recorded>>>) -> Self {
        Self { calls, panic_once: Arc::new(Mutex::new(false)), sleep: Duration::ZERO }
    }
}

#[async_trait]
impl TtsProvider for FakeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAi
    }

    async fn is_available(&self) -> bool {
        true
    }

    async fn synthesize(
        &self,
        speech: &ResolvedSpeech,
        cancel: &CancelToken,
    ) -> Result<SpeechOutput, ProviderError> {
        if std::mem::replace(&mut *self.panic_once.lock().unwrap(), false) {
            panic!("fake provider panic for test");
        }
        self.calls.lock().unwrap().push(Recorded {
            text: speech.text.clone(),
            voice_id: speech.voice_id.clone(),
            rate: speech.rate,
            volume: speech.volume,
        });
        let deadline = std::time::Instant::now() + self.sleep;
        while std::time::Instant::now() < deadline {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        Ok(SpeechOutput::PlayedInline)
    }

    async fn list_voices(&self) -> Vec<VoiceInfo> {
        Vec::new()
    }
}

fn config() -> AppConfig {
    let mut config = AppConfig::default();
    config.tts.default_voice = "default".to_string();
    config.voices.insert(
        "default".to_string(),
        VoiceConfig {
            provider: ProviderKind::OpenAi,
            voice: Some("alloy".to_string()),
            language: None,
            rate: None,
            pitch: None,
            volume: Some(0.5),
        },
    );
    config
}

fn engine_with(provider: Arc<FakeProvider>, config: AppConfig) -> SpeechEngine {
    let mut providers: HashMap<ProviderKind, Arc<dyn TtsProvider>> = HashMap::new();
    providers.insert(ProviderKind::OpenAi, provider);
    let registry = ProviderRegistry::from_providers(providers);
    SpeechEngine::with_registry(Arc::new(config), registry)
}

fn request(text: &str) -> SpeakRequest {
    SpeakRequest { text: text.to_string(), voice: None, rate: None }
}

#[tokio::test]
async fn enqueue_and_wait_completes_with_recorded_volume() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = engine_with(Arc::new(FakeProvider::new(calls.clone())), config());
    let _worker = engine.start();

    let (_id, rx) = engine.enqueue_and_wait(request("hello")).expect("enqueue");
    assert_eq!(rx.await.expect("outcome"), JobOutcome::Completed);

    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].text, "hello");
    assert_eq!(recorded[0].voice_id.as_deref(), Some("alloy"));
    assert_eq!(recorded[0].volume, 0.5);
}

#[tokio::test]
async fn fire_and_forget_drains_to_idle() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = engine_with(Arc::new(FakeProvider::new(calls)), config());
    let _worker = engine.start();

    engine.enqueue(request("hi")).expect("enqueue");
    for _ in 0..200 {
        if !engine.status().state.is_speaking() && engine.queue_depth() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(engine.queue_depth(), 0);
    assert!(!engine.status().state.is_speaking());
}

#[tokio::test]
async fn stop_cancels_active_job() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut provider = FakeProvider::new(calls);
    provider.sleep = Duration::from_secs(30);
    let engine = engine_with(Arc::new(provider), config());
    let _worker = engine.start();

    let (_id, rx) = engine.enqueue_and_wait(request("slow")).expect("enqueue");
    for _ in 0..200 {
        if engine.status().state.is_speaking() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    engine.stop();
    assert_eq!(rx.await.expect("outcome"), JobOutcome::Cancelled);
}

#[tokio::test]
async fn queue_full_is_rejected() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut provider = FakeProvider::new(calls);
    provider.sleep = Duration::from_secs(30);
    let mut config = config();
    config.tts.max_queue_depth = 1;
    let engine = engine_with(Arc::new(provider), config);
    let _worker = engine.start();

    // First job becomes active; allow the worker to dequeue it.
    engine.enqueue(request("one")).expect("first enqueue");
    for _ in 0..200 {
        if engine.status().state.is_speaking() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Fill the single queue slot, then the next enqueue must be rejected.
    engine.enqueue(request("two")).expect("second enqueue fills queue");
    let error = engine.enqueue(request("three")).expect_err("queue should be full");
    assert!(matches!(error, EnqueueError::QueueFull { capacity: 1, .. }));
}

#[tokio::test]
async fn panicking_job_degrades_then_recovers() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider = FakeProvider::new(calls.clone());
    *provider.panic_once.lock().unwrap() = true;
    let engine = engine_with(Arc::new(provider), config());
    let _worker = engine.start();

    let (_id, rx) = engine.enqueue_and_wait(request("boom")).expect("enqueue");
    assert!(matches!(rx.await.expect("outcome"), JobOutcome::Failed { .. }));
    for _ in 0..200 {
        if engine.status().degraded {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(engine.status().degraded, "engine should be degraded after a panic");

    let (_id, rx) = engine.enqueue_and_wait(request("after")).expect("enqueue");
    assert_eq!(rx.await.expect("outcome"), JobOutcome::Completed);
    assert_eq!(calls.lock().unwrap().last().map(|r| r.text.as_str()), Some("after"));
}
