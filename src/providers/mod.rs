//! TTS provider abstraction and implementations (PLAN.md task 6).
//!
//! Defines the `TtsProvider` trait and the Apple (local), OpenAI, and Google backends, plus
//! local-first routing/fallback. Apple is macOS-only; cloud providers are cross-platform.

pub mod google;
pub mod openai;

#[cfg(target_os = "macos")]
pub mod apple;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;
use tracing::{info, warn};

use crate::config::{AppConfig, ProviderKind};
use crate::voices::ResolvedSpeech;
use crate::TARGET_PROVIDER;

/// A cooperative cancellation flag shared with providers and the playback sink.
///
/// Providers and the rodio playback loop poll [`CancelToken::is_cancelled`] at boundaries and
/// abort promptly when set (the worker sets it on `stop`).
#[derive(Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// Create a fresh, un-cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// The result of a provider synthesizing an utterance.
pub enum SpeechOutput {
    /// The provider played the audio itself (e.g. the Apple `say` path); nothing more to do.
    PlayedInline,
    /// Encoded audio bytes (WAV/MP3/…) for the shared rodio sink to decode and play.
    Audio(Vec<u8>),
}

/// A voice advertised by a provider, surfaced through `list_voices`.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct VoiceInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// Errors a provider can raise while synthesizing speech.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider {0} is unavailable")]
    Unavailable(ProviderKind),
    #[error("missing credentials for {0}")]
    MissingCredentials(ProviderKind),
    #[error("{provider} request failed: {message}")]
    Request { provider: ProviderKind, message: String },
    #[error("speech was cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

/// A text-to-speech backend.
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Which backend this is.
    fn kind(&self) -> ProviderKind;

    /// Whether this provider can currently service requests (credentials/platform present).
    async fn is_available(&self) -> bool;

    /// Synthesize (and, for self-playing backends, play) the resolved utterance.
    async fn synthesize(
        &self,
        speech: &ResolvedSpeech,
        cancel: &CancelToken,
    ) -> Result<SpeechOutput, ProviderError>;

    /// Best-effort enumeration of provider voices (empty if unsupported/unavailable).
    async fn list_voices(&self) -> Vec<VoiceInfo>;
}

/// Holds the configured providers and performs local-first routing with fallback.
pub struct ProviderRegistry {
    providers: HashMap<ProviderKind, Arc<dyn TtsProvider>>,
}

impl ProviderRegistry {
    /// Build a registry from explicitly supplied providers (primarily for in-crate tests).
    pub fn from_providers(providers: HashMap<ProviderKind, Arc<dyn TtsProvider>>) -> Self {
        Self { providers }
    }

    /// Build the registry from configuration. Apple is only present on macOS.
    pub fn from_config(config: &AppConfig) -> Self {
        let mut providers: HashMap<ProviderKind, Arc<dyn TtsProvider>> = HashMap::new();

        #[cfg(target_os = "macos")]
        providers.insert(
            ProviderKind::Apple,
            Arc::new(apple::AppleProvider::new(config.providers.apple.clone())),
        );

        providers.insert(
            ProviderKind::OpenAi,
            Arc::new(openai::OpenAiProvider::new(
                config.providers.openai.clone(),
                config.resolved_openai_api_key(),
            )),
        );
        providers.insert(
            ProviderKind::Google,
            Arc::new(google::GoogleProvider::new(
                config.providers.google.clone(),
                config.resolved_google_api_key(),
            )),
        );

        Self::from_providers(providers)
    }

    /// Look up a provider by kind.
    pub fn get(&self, kind: ProviderKind) -> Option<Arc<dyn TtsProvider>> {
        self.providers.get(&kind).cloned()
    }

    /// All configured provider kinds (unordered).
    pub fn kinds(&self) -> Vec<ProviderKind> {
        self.providers.keys().copied().collect()
    }

    /// Select a provider for the resolved speech, honoring availability and the fallback
    /// `order`. If the primary provider is unavailable, the first available provider in
    /// `order` is used with its default voice (the provider-specific voice id is dropped).
    pub async fn select(
        &self,
        resolved: &ResolvedSpeech,
        order: &[ProviderKind],
    ) -> Option<(Arc<dyn TtsProvider>, ResolvedSpeech)> {
        if let Some(provider) = self.get(resolved.provider) {
            if provider.is_available().await {
                return Some((provider, resolved.clone()));
            }
        } else {
            warn!(
                target: TARGET_PROVIDER,
                requested = ?resolved.provider,
                voice = resolved.voice_id.as_deref().unwrap_or("<provider-default>"),
                "preferred voice provider is not configured; considering fallback providers"
            );
        }

        for &kind in order {
            if kind == resolved.provider {
                continue;
            }
            if let Some(provider) = self.get(kind) {
                if provider.is_available().await {
                    let mut fallback = resolved.clone();
                    fallback.provider = kind;
                    fallback.voice_id = None;
                    info!(
                        target: TARGET_PROVIDER,
                        requested = ?resolved.provider,
                        substituted = ?kind,
                        voice = resolved.voice_id.as_deref().unwrap_or("<provider-default>"),
                        "voice/provider unavailable; silently substituting"
                    );
                    return Some((provider, fallback));
                }
            }
        }

        None
    }

    /// Enumerate voices for every configured provider.
    pub async fn list_all_voices(&self) -> Vec<(ProviderKind, Vec<VoiceInfo>)> {
        let mut out = Vec::new();
        for (kind, provider) in &self.providers {
            out.push((*kind, provider.list_voices().await));
        }
        out.sort_by_key(|(kind, _)| kind.as_str());
        out
    }
}
