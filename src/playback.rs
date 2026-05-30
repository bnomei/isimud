//! Audio playback via `rodio` (PLAN.md task 5).
//!
//! Decodes and plays the WAV/MP3/PCM bytes returned by cloud providers. The Apple provider
//! plays itself via the `say` CLI, so both paths share the same speech state/pulse.

use std::io::Cursor;
use std::time::Duration;

use thiserror::Error;

use crate::providers::CancelToken;

/// Poll interval while waiting for a clip to finish (also the cancellation latency).
const POLL_INTERVAL: Duration = Duration::from_millis(40);

/// Errors raised while decoding or playing audio bytes.
#[derive(Debug, Error)]
pub enum PlaybackError {
    #[error("no audio output device available: {0}")]
    Device(String),
    #[error("failed to decode or play audio: {0}")]
    Decode(String),
    #[error("playback was cancelled")]
    Cancelled,
}

/// Decode and play encoded audio bytes, blocking (on a worker thread) until the clip ends or
/// `cancel` is tripped. rodio auto-detects the container/codec from the byte stream.
pub async fn play_audio(bytes: Vec<u8>, cancel: CancelToken) -> Result<(), PlaybackError> {
    tokio::task::spawn_blocking(move || play_blocking(bytes, &cancel))
        .await
        .map_err(|error| PlaybackError::Decode(format!("playback task join error: {error}")))?
}

fn play_blocking(bytes: Vec<u8>, cancel: &CancelToken) -> Result<(), PlaybackError> {
    if cancel.is_cancelled() {
        return Err(PlaybackError::Cancelled);
    }

    let mut device = rodio::DeviceSinkBuilder::open_default_sink()
        .map_err(|error| PlaybackError::Device(error.to_string()))?;
    // We keep `device` alive for the lifetime of playback; silence the drop warning.
    device.log_on_drop(false);

    let player = rodio::play(device.mixer(), Cursor::new(bytes))
        .map_err(|error| PlaybackError::Decode(error.to_string()))?;

    loop {
        if cancel.is_cancelled() {
            player.stop();
            return Err(PlaybackError::Cancelled);
        }
        if player.empty() {
            return Ok(());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}
