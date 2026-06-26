//! isimud — macOS menu bar text-to-speech and MCP server for AI agents.
//!
//! The functional inverse of MUNINN (speech-to-text): agents enqueue speech through MCP
//! tools; a single worker serializes playback, routes named voices to TTS providers, and
//! broadcasts lifecycle events to the menu-bar indicator and connected MCP peers.

/// TOML configuration loading, validation, and credential resolution.
pub mod config;
/// MCP tool handlers and speech-event notification fan-out.
pub mod mcp;
/// Shared rodio playback for cloud-provider audio bytes.
pub mod playback;
/// TTS provider trait, registry, and Apple/OpenAI/Google backends.
pub mod providers;
/// Axum HTTP server wiring for streamable MCP over `/mcp`.
pub mod server;
/// Speech state machine, status snapshots, and lifecycle events.
pub mod state;
/// Named-voice resolution from `[voices.*]` into provider parameters.
pub mod voices;
/// Serialized speech queue, worker task, and engine API.
pub mod worker;

/// Re-export of the top-level TOML configuration type.
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
