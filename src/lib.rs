//! isimud — macOS menu bar text-to-speech and MCP server for AI agents.
//!
//! The functional inverse of MUNINN (speech-to-text): isimud lets an agent speak by
//! sending text to an MCP tool that synthesizes and plays it aloud. See `PLAN.md` for the
//! agreed brief and build checklist.

pub mod config;
pub mod error;
pub mod mcp;
pub mod playback;
pub mod providers;
pub mod server;
pub mod state;
pub mod voices;
pub mod worker;

pub use config::AppConfig;

/// Tracing target for runtime/lifecycle events.
pub const TARGET_RUNTIME: &str = "runtime";
/// Tracing target for the MCP/HTTP server.
pub const TARGET_SERVER: &str = "server";
/// Tracing target for TTS provider activity.
pub const TARGET_PROVIDER: &str = "provider";
/// Tracing target for configuration handling.
pub const TARGET_CONFIG: &str = "config";
/// Tracing target for speech worker / playback activity.
pub const TARGET_SPEECH: &str = "speech";
/// Catch-all tracing target.
pub const TARGET_DEFAULT: &str = "default";

/// Default MCP server port — T9 keypad spelling of "ENKI" (the god isimud serves),
/// inside the IANA registered range (1024–49151).
pub const DEFAULT_PORT: u16 = 3654;

/// Default loopback bind address for the MCP/HTTP server.
pub const DEFAULT_BIND_HOST: &str = "127.0.0.1";
