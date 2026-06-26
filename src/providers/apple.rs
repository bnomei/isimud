//! Apple local TTS provider (PLAN.md task 6, macOS only).
//!
//! Synthesizes and plays speech via the macOS `say` CLI (cancellable, headless-safe, no run
//! loop required) and enumerates installed voices natively through `objc2-avf-audio`
//! `AVSpeechSynthesisVoice`. It plays its own audio, so it bypasses the rodio sink and cannot
//! currently apply resolved volume or pitch.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use objc2_avf_audio::AVSpeechSynthesisVoice;
use tracing::debug;

use super::{CancelToken, ProviderError, SpeechOutput, TtsProvider, VoiceInfo};
use crate::config::{AppleProviderConfig, ProviderKind};
use crate::voices::ResolvedSpeech;
use crate::TARGET_PROVIDER;

/// Words-per-minute that corresponds to a neutral rate multiplier of `1.0`.
const NEUTRAL_WPM: f32 = 175.0;
/// Poll interval while waiting for the `say` child (also the cancellation latency).
const POLL_INTERVAL: Duration = Duration::from_millis(40);
static LOGGED_UNSUPPORTED_CONTROLS: AtomicBool = AtomicBool::new(false);

/// Apple local TTS backend.
pub struct AppleProvider {
    config: AppleProviderConfig,
}

impl AppleProvider {
    /// Build the provider from its `[providers.apple]` config.
    pub fn new(config: AppleProviderConfig) -> Self {
        Self { config }
    }

    /// Map a neutral rate multiplier (1.0 = normal) to a `say -r` words-per-minute value.
    fn words_per_minute(rate: f32) -> u32 {
        // A NaN multiplier (e.g. a `rate = nan` TOML literal that bypasses the URL boundary
        // guard) would slip through clamp() untouched — all its comparisons are false — and
        // then `NaN as u32` saturates to 0, producing an invalid `say -r 0`. Fall back to the
        // neutral multiplier so the result always lands in the [80, 500] WPM band.
        let rate = if rate.is_nan() { 1.0 } else { rate };
        let wpm = (NEUTRAL_WPM * rate).round();
        wpm.clamp(80.0, 500.0) as u32
    }
}

#[async_trait]
impl TtsProvider for AppleProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Apple
    }

    async fn is_available(&self) -> bool {
        // The `say` binary ships with macOS.
        true
    }

    async fn synthesize(
        &self,
        speech: &ResolvedSpeech,
        cancel: &CancelToken,
    ) -> Result<SpeechOutput, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }

        if (speech.volume != 1.0 || speech.pitch.is_some())
            && !LOGGED_UNSUPPORTED_CONTROLS.swap(true, Ordering::SeqCst)
        {
            debug!(
                target: TARGET_PROVIDER,
                volume = speech.volume,
                pitch = ?speech.pitch,
                "Apple say provider does not apply resolved volume or pitch"
            );
        }

        let mut command = tokio::process::Command::new("say");
        command.arg("-r").arg(Self::words_per_minute(speech.rate).to_string());

        if let Some(voice) = speech.voice_id.as_deref() {
            command.arg("-v").arg(voice);
        }
        let _ = &self.config; // language hint is conveyed via the named voice id.
        command.arg("--").arg(&speech.text);

        let mut child = command.spawn().map_err(|error| ProviderError::Request {
            provider: ProviderKind::Apple,
            message: format!("failed to launch `say`: {error}"),
        })?;

        loop {
            if cancel.is_cancelled() {
                let _ = child.kill().await;
                return Err(ProviderError::Cancelled);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        return Ok(SpeechOutput::PlayedInline);
                    }
                    return Err(ProviderError::Request {
                        provider: ProviderKind::Apple,
                        message: format!("`say` exited with status {status}"),
                    });
                }
                Ok(None) => tokio::time::sleep(POLL_INTERVAL).await,
                Err(error) => {
                    return Err(ProviderError::Request {
                        provider: ProviderKind::Apple,
                        message: format!("waiting on `say`: {error}"),
                    })
                }
            }
        }
    }

    async fn list_voices(&self) -> Vec<VoiceInfo> {
        // `AVSpeechSynthesisVoice::speechVoices` is a class method safe to call off the main
        // thread; the returned Objective-C objects are used synchronously (no await held).
        let voices = unsafe { AVSpeechSynthesisVoice::speechVoices() };
        let mut out = Vec::with_capacity(voices.len());
        for voice in &voices {
            let id = unsafe { voice.identifier() }.to_string();
            let name = unsafe { voice.name() }.to_string();
            let language = unsafe { voice.language() }.to_string();
            out.push(VoiceInfo { id, name, language: Some(language) });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::AppleProvider;

    #[test]
    fn neutral_rate_maps_to_baseline_wpm() {
        assert_eq!(AppleProvider::words_per_minute(1.0), 175);
    }

    #[test]
    fn rate_is_clamped_to_say_bounds() {
        assert_eq!(AppleProvider::words_per_minute(0.01), 80);
        assert_eq!(AppleProvider::words_per_minute(100.0), 500);
    }

    #[test]
    fn non_finite_rate_stays_in_band() {
        // NaN must not slip through clamp() and saturate to `say -r 0`; it falls back to
        // neutral. ±inf already clamp to the band bounds.
        assert_eq!(AppleProvider::words_per_minute(f32::NAN), 175);
        assert_eq!(AppleProvider::words_per_minute(f32::INFINITY), 500);
        assert_eq!(AppleProvider::words_per_minute(f32::NEG_INFINITY), 80);
    }
}
