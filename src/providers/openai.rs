//! OpenAI TTS provider (PLAN.md task 6).
//!
//! Calls `POST /v1/audio/speech` (`gpt-4o-mini-tts`) requesting WAV bytes, then hands them to
//! the rodio playback sink. BYOK via `OPENAI_API_KEY` with config fallback.

use async_trait::async_trait;
use serde_json::json;

use super::{CancelToken, ProviderError, SpeechOutput, TtsProvider, VoiceInfo};
use crate::config::{OpenAiProviderConfig, ProviderKind};
use crate::voices::ResolvedSpeech;

/// Default voice when a named voice does not specify a provider voice id.
const DEFAULT_VOICE: &str = "alloy";
/// Built-in voices advertised by the OpenAI speech API.
const KNOWN_VOICES: &[&str] = &[
    "alloy", "ash", "ballad", "cedar", "coral", "echo", "fable", "marin", "nova", "onyx", "sage",
    "shimmer", "verse",
];

/// OpenAI cloud TTS backend.
pub struct OpenAiProvider {
    config: OpenAiProviderConfig,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// Build the provider with its config and resolved API key (env or config).
    pub fn new(config: OpenAiProviderConfig, api_key: Option<String>) -> Self {
        Self { config, api_key, client: reqwest::Client::new() }
    }
}

#[async_trait]
impl TtsProvider for OpenAiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAi
    }

    async fn is_available(&self) -> bool {
        self.api_key.is_some()
    }

    async fn synthesize(
        &self,
        speech: &ResolvedSpeech,
        cancel: &CancelToken,
    ) -> Result<SpeechOutput, ProviderError> {
        let api_key =
            self.api_key.as_ref().ok_or(ProviderError::MissingCredentials(ProviderKind::OpenAi))?;

        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }

        let voice = speech.voice_id.as_deref().unwrap_or(DEFAULT_VOICE);
        let mut body = json!({
            "model": self.config.model,
            "input": speech.text,
            "voice": voice,
            "response_format": self.config.response_format,
        });
        // `rate` is a neutral multiplier (1.0 = normal); OpenAI accepts speed in [0.25, 4.0].
        if (0.25..=4.0).contains(&speech.rate) {
            body["speed"] = json!(speech.rate);
        }

        let response = self
            .client
            .post(&self.config.endpoint)
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| ProviderError::Request {
                provider: ProviderKind::OpenAi,
                message: error.to_string(),
            })?;

        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(ProviderError::Request {
                provider: ProviderKind::OpenAi,
                message: format!("status {status}: {}", detail.trim()),
            });
        }

        let bytes = response.bytes().await.map_err(|error| ProviderError::Request {
            provider: ProviderKind::OpenAi,
            message: format!("reading audio body: {error}"),
        })?;

        Ok(SpeechOutput::Audio(bytes.to_vec()))
    }

    async fn list_voices(&self) -> Vec<VoiceInfo> {
        KNOWN_VOICES
            .iter()
            .map(|name| VoiceInfo {
                id: (*name).to_string(),
                name: (*name).to_string(),
                language: None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OpenAiProviderConfig;

    fn provider(api_key: Option<String>) -> OpenAiProvider {
        OpenAiProvider::new(OpenAiProviderConfig::default(), api_key)
    }

    #[test]
    fn kind_is_openai() {
        assert_eq!(provider(None).kind(), ProviderKind::OpenAi);
    }

    #[tokio::test]
    async fn availability_tracks_api_key_presence() {
        assert!(!provider(None).is_available().await);
        assert!(provider(Some("sk-test".to_string())).is_available().await);
    }

    #[tokio::test]
    async fn list_voices_advertises_known_voices() {
        let voices = provider(None).list_voices().await;
        assert_eq!(voices.len(), KNOWN_VOICES.len());
        assert!(voices.iter().any(|voice| voice.id == DEFAULT_VOICE));
        assert!(voices.iter().all(|voice| voice.language.is_none()));
    }
}
