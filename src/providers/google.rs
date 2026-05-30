//! Google Cloud Text-to-Speech provider (PLAN.md task 6).
//!
//! Calls `POST text:synthesize?key=...` requesting LINEAR16 audio, base64-decodes the
//! `audioContent`, and hands it to the rodio playback sink. BYOK via `GOOGLE_API_KEY`.

use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::json;

use super::{CancelToken, ProviderError, SpeechOutput, TtsProvider, VoiceInfo};
use crate::config::{GoogleProviderConfig, ProviderKind};
use crate::voices::ResolvedSpeech;

#[derive(Deserialize)]
struct SynthesizeResponse {
    #[serde(rename = "audioContent")]
    audio_content: String,
}

/// Google Cloud TTS backend.
pub struct GoogleProvider {
    config: GoogleProviderConfig,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl GoogleProvider {
    /// Build the provider with its config and resolved API key (env or config).
    pub fn new(config: GoogleProviderConfig, api_key: Option<String>) -> Self {
        Self { config, api_key, client: reqwest::Client::new() }
    }
}

#[async_trait]
impl TtsProvider for GoogleProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Google
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
            self.api_key.as_ref().ok_or(ProviderError::MissingCredentials(ProviderKind::Google))?;

        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }

        let language = speech.language.clone().unwrap_or_else(|| self.config.language.clone());

        let mut voice = json!({ "languageCode": language });
        if let Some(name) = speech.voice_id.as_deref() {
            voice["name"] = json!(name);
        }

        let mut audio_config = json!({ "audioEncoding": self.config.audio_encoding });
        // `rate` is a neutral multiplier (1.0 = normal); Google speakingRate is [0.25, 4.0].
        if (0.25..=4.0).contains(&speech.rate) {
            audio_config["speakingRate"] = json!(speech.rate);
        }
        if let Some(pitch) = speech.pitch {
            if (-20.0..=20.0).contains(&pitch) {
                audio_config["pitch"] = json!(pitch);
            }
        }

        let body = json!({
            "input": { "text": speech.text },
            "voice": voice,
            "audioConfig": audio_config,
        });

        let response = self
            .client
            .post(&self.config.endpoint)
            .query(&[("key", api_key.as_str())])
            .json(&body)
            .send()
            .await
            .map_err(|error| ProviderError::Request {
                provider: ProviderKind::Google,
                message: error.to_string(),
            })?;

        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(ProviderError::Request {
                provider: ProviderKind::Google,
                message: format!("status {status}: {}", detail.trim()),
            });
        }

        let parsed: SynthesizeResponse =
            response.json().await.map_err(|error| ProviderError::Request {
                provider: ProviderKind::Google,
                message: format!("parsing synthesize response: {error}"),
            })?;

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(parsed.audio_content.as_bytes())
            .map_err(|error| ProviderError::Request {
                provider: ProviderKind::Google,
                message: format!("decoding audioContent base64: {error}"),
            })?;

        Ok(SpeechOutput::Audio(bytes))
    }

    async fn list_voices(&self) -> Vec<VoiceInfo> {
        // Voice enumeration requires a separate authenticated catalog call; treat as
        // best-effort and return an empty list (named voices remain available via config).
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GoogleProviderConfig;

    fn provider(api_key: Option<String>) -> GoogleProvider {
        GoogleProvider::new(GoogleProviderConfig::default(), api_key)
    }

    #[test]
    fn kind_is_google() {
        assert_eq!(provider(None).kind(), ProviderKind::Google);
    }

    #[tokio::test]
    async fn availability_tracks_api_key_presence() {
        assert!(!provider(None).is_available().await);
        assert!(provider(Some("key".to_string())).is_available().await);
    }

    #[tokio::test]
    async fn list_voices_is_empty_best_effort() {
        assert!(provider(None).list_voices().await.is_empty());
    }
}
