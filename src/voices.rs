//! Named-voice resolution from agent requests into provider parameters.
//!
//! Maps a [`SpeakRequest`] through `[voices.<name>]` and `[tts].default_voice` into a
//! [`ResolvedSpeech`] with effective rate, volume, and pitch. Provider fallback order is
//! applied later in the registry; volume reaches rodio for cloud audio but not Apple `say`,
//! and pitch is honored only by Google.

use thiserror::Error;

use crate::config::{AppConfig, ProviderKind};

/// An agent's request to speak some text, before voice resolution.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakRequest {
    /// The text to speak.
    pub text: String,
    /// Optional named voice (`[voices.<name>]`). Falls back to `[tts].default_voice`.
    pub voice: Option<String>,
    /// Optional speaking-rate override.
    pub rate: Option<f32>,
}

/// A fully resolved speech job: concrete provider plus provider-specific parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSpeech {
    /// Backend that will synthesize/play this utterance.
    pub provider: ProviderKind,
    /// The named voice that was selected.
    pub voice_name: String,
    /// Provider-specific voice id/name (None = provider default).
    pub voice_id: Option<String>,
    /// BCP-47 language hint, if configured.
    pub language: Option<String>,
    /// Effective speaking rate.
    pub rate: f32,
    /// Optional pitch multiplier.
    pub pitch: Option<f32>,
    /// Effective playback volume. The shared rodio path honors this; Apple `say` cannot.
    pub volume: f32,
    /// The text to speak.
    pub text: String,
}

/// Errors that can occur while resolving a named voice.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VoiceResolveError {
    #[error("unknown voice '{0}' (not present in [voices.*])")]
    UnknownVoice(String),
    #[error("speak text must not be empty")]
    EmptyText,
}

/// Resolve a [`SpeakRequest`] against the configuration.
///
/// The requested voice (or `[tts].default_voice`) must exist in `[voices.*]`. The effective
/// rate is the request override, then the voice's `rate`, then `[tts].rate`. Effective volume
/// comes from the voice's `volume`, falling back to neutral `1.0`.
pub fn resolve_speech(
    config: &AppConfig,
    request: &SpeakRequest,
) -> Result<ResolvedSpeech, VoiceResolveError> {
    if request.text.trim().is_empty() {
        return Err(VoiceResolveError::EmptyText);
    }

    let voice_name = request.voice.clone().unwrap_or_else(|| config.tts.default_voice.clone());

    let voice = config
        .voices
        .get(&voice_name)
        .ok_or_else(|| VoiceResolveError::UnknownVoice(voice_name.clone()))?;

    let rate = request.rate.or(voice.rate).unwrap_or(config.tts.rate);
    let volume = voice.volume.unwrap_or(1.0);

    let language = voice.language.clone().or_else(|| match voice.provider {
        ProviderKind::Apple => config.providers.apple.language.clone(),
        ProviderKind::Google => Some(config.providers.google.language.clone()),
        ProviderKind::OpenAi => None,
    });

    Ok(ResolvedSpeech {
        provider: voice.provider,
        voice_name,
        voice_id: voice.voice.clone(),
        language,
        rate,
        pitch: voice.pitch,
        volume,
        text: request.text.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::{resolve_speech, SpeakRequest, VoiceResolveError};
    use crate::config::{AppConfig, ProviderKind, VoiceConfig};

    fn config_with_voices() -> AppConfig {
        let mut config = AppConfig::default();
        config.tts.default_voice = "default".to_string();
        config.tts.rate = 0.5;
        config.voices.insert(
            "default".to_string(),
            VoiceConfig {
                provider: ProviderKind::Apple,
                voice: Some("Samantha".to_string()),
                language: None,
                rate: None,
                pitch: None,
                volume: None,
            },
        );
        config.voices.insert(
            "narrator".to_string(),
            VoiceConfig {
                provider: ProviderKind::OpenAi,
                voice: Some("onyx".to_string()),
                language: None,
                rate: Some(0.9),
                pitch: None,
                volume: None,
            },
        );
        config
    }

    #[test]
    fn resolves_default_voice_when_unspecified() {
        let config = config_with_voices();
        let request = SpeakRequest { text: "hello".to_string(), voice: None, rate: None };
        let resolved = resolve_speech(&config, &request).expect("default voice resolves");
        assert_eq!(resolved.provider, ProviderKind::Apple);
        assert_eq!(resolved.voice_name, "default");
        assert_eq!(resolved.voice_id.as_deref(), Some("Samantha"));
        assert_eq!(resolved.rate, 0.5);
    }

    #[test]
    fn request_rate_overrides_voice_and_global_rate() {
        let config = config_with_voices();
        let request = SpeakRequest {
            text: "hi".to_string(),
            voice: Some("narrator".to_string()),
            rate: Some(0.25),
        };
        let resolved = resolve_speech(&config, &request).expect("narrator resolves");
        assert_eq!(resolved.provider, ProviderKind::OpenAi);
        assert_eq!(resolved.rate, 0.25);
    }

    #[test]
    fn voice_rate_used_when_request_rate_absent() {
        let config = config_with_voices();
        let request = SpeakRequest {
            text: "hi".to_string(),
            voice: Some("narrator".to_string()),
            rate: None,
        };
        let resolved = resolve_speech(&config, &request).expect("narrator resolves");
        assert_eq!(resolved.rate, 0.9);
    }

    #[test]
    fn unknown_voice_is_rejected() {
        let config = config_with_voices();
        let request =
            SpeakRequest { text: "hi".to_string(), voice: Some("ghost".to_string()), rate: None };
        let error = resolve_speech(&config, &request).expect_err("unknown voice fails");
        assert_eq!(error, VoiceResolveError::UnknownVoice("ghost".to_string()));
    }

    #[test]
    fn empty_text_is_rejected() {
        let config = config_with_voices();
        let request = SpeakRequest { text: "   ".to_string(), voice: None, rate: None };
        assert_eq!(
            resolve_speech(&config, &request).expect_err("empty text fails"),
            VoiceResolveError::EmptyText
        );
    }
}
