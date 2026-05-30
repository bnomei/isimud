//! Error types for isimud.
//!
//! Filled in alongside the config, provider, and worker modules (PLAN.md tasks 2–6).

use thiserror::Error;

/// Top-level error type for isimud operations.
#[derive(Debug, Error)]
pub enum IsimudError {
    /// A TTS provider failed to synthesize or play speech.
    #[error("tts provider error: {0}")]
    Provider(String),
}
