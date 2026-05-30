//! Configuration loading and resolution (PLAN.md task 2).
//!
//! Mirrors MUNINN's TOML + XDG path resolution and secrets handling. Sections:
//! `[app] [server] [tts] [voices.*] [providers.*] [logging]`.
//!
//! Path precedence: `ISIMUD_CONFIG` -> `$XDG_CONFIG_HOME/isimud/config.toml`
//! -> `~/.config/isimud/config.toml`.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{DEFAULT_BIND_HOST, DEFAULT_PORT};

const DEFAULT_CONFIG_DIR_NAME: &str = "isimud";
const DEFAULT_CONFIG_FILE_NAME: &str = "config.toml";

/// Environment variable holding an explicit config path override.
pub const ENV_CONFIG_PATH: &str = "ISIMUD_CONFIG";
/// Environment variable holding the MCP bearer auth token.
pub const ENV_AUTH_TOKEN: &str = "ISIMUD_AUTH_TOKEN";
/// Environment variable holding the OpenAI API key.
pub const ENV_OPENAI_API_KEY: &str = "OPENAI_API_KEY";
/// Environment variable holding the Google Cloud API key.
pub const ENV_GOOGLE_API_KEY: &str = "GOOGLE_API_KEY";

/// Top-level application configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub app: AppSettings,
    pub server: ServerConfig,
    pub tts: TtsConfig,
    #[serde(default)]
    pub voices: BTreeMap<String, VoiceConfig>,
    pub providers: ProvidersConfig,
    pub logging: LoggingConfig,
}

/// `[app]` — runtime / menu-bar behavior.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AppSettings {
    /// Run the menu bar tray. False (or `--headless`) yields a pure MCP server.
    pub menubar: bool,
    /// Install/remove a macOS LaunchAgent so isimud starts at login.
    pub autostart: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self { menubar: true, autostart: false }
    }
}

/// `[server]` — MCP streamable-HTTP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub path: String,
    /// Optional bearer token required on MCP requests. Empty/None = no auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    /// Allow binding to a non-loopback address (requires `auth_token`).
    pub allow_remote: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_BIND_HOST.to_string(),
            port: DEFAULT_PORT,
            path: "/mcp".to_string(),
            auth_token: None,
            allow_remote: false,
        }
    }
}

/// `[tts]` — global speech defaults and provider routing order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TtsConfig {
    /// Availability / fallback order when a named voice's provider is unavailable.
    pub providers: Vec<ProviderKind>,
    /// Named voice used for a plain `speak(text)` call with no explicit voice.
    pub default_voice: String,
    /// Global default speaking rate as a neutral multiplier (1.0 = normal).
    pub rate: f32,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            providers: vec![ProviderKind::Apple, ProviderKind::OpenAi, ProviderKind::Google],
            default_voice: "default".to_string(),
            rate: 1.0,
        }
    }
}

/// Identifies a TTS backend.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    Hash,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Apple,
    #[serde(rename = "openai")]
    OpenAi,
    Google,
}

impl ProviderKind {
    /// Stable string identifier used in config, logs, and MCP responses.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Apple => "apple",
            ProviderKind::OpenAi => "openai",
            ProviderKind::Google => "google",
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `[voices.<name>]` — a named voice mapping to a provider + provider voice id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VoiceConfig {
    pub provider: ProviderKind,
    /// Provider-specific voice id/name. Omit to use the provider's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
}

/// `[providers.*]` — non-voice provider settings (endpoints, models, credentials).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProvidersConfig {
    pub apple: AppleProviderConfig,
    pub openai: OpenAiProviderConfig,
    pub google: GoogleProviderConfig,
}

/// `[providers.apple]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AppleProviderConfig {
    /// BCP-47 language for the system default voice when `voice` is unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// `[providers.openai]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct OpenAiProviderConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub endpoint: String,
    pub model: String,
    pub response_format: String,
}

impl Default for OpenAiProviderConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            endpoint: "https://api.openai.com/v1/audio/speech".to_string(),
            model: "gpt-4o-mini-tts".to_string(),
            response_format: "wav".to_string(),
        }
    }
}

/// `[providers.google]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct GoogleProviderConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub endpoint: String,
    pub language: String,
    pub audio_encoding: String,
}

impl Default for GoogleProviderConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            endpoint: "https://texttospeech.googleapis.com/v1/text:synthesize".to_string(),
            language: "en-US".to_string(),
            audio_encoding: "LINEAR16".to_string(),
        }
    }
}

/// `[logging]` — honors `RUST_LOG`; defaults to `info`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self { level: "info".to_string() }
    }
}

impl AppConfig {
    /// Load configuration from the resolved path, creating a default file if absent.
    pub fn load() -> Result<Self, ConfigError> {
        let path = resolve_config_path()?;
        Self::load_or_create_default(path)
    }

    /// Load configuration from an explicit path. Errors if the file is missing.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(ConfigError::NotFound { path: path.to_path_buf() });
        }

        let raw = fs::read_to_string(path)
            .map_err(|source| ConfigError::Read { path: path.to_path_buf(), source })?;

        toml::from_str(&raw)
            .map_err(|source| ConfigError::ParseTomlAtPath { path: path.to_path_buf(), source })
    }

    /// Parse configuration from a TOML string (used in tests).
    pub fn from_toml_str(raw: &str) -> Result<Self, ConfigError> {
        toml::from_str(raw).map_err(|source| ConfigError::ParseToml { source })
    }

    /// The configuration written when no file exists yet.
    pub fn launchable_default() -> Self {
        let mut config = Self::default();
        config.voices.insert(
            "default".to_string(),
            VoiceConfig {
                provider: ProviderKind::Apple,
                voice: None,
                language: None,
                rate: None,
                pitch: None,
                volume: None,
            },
        );
        config
    }

    fn load_or_create_default(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            write_default_config(path)?;
        }
        Self::load_from_path(path)
    }

    /// Resolve the MCP bearer token: env (`ISIMUD_AUTH_TOKEN`) overrides config.
    pub fn resolved_auth_token(&self) -> Option<String> {
        non_empty_env(ENV_AUTH_TOKEN).or_else(|| {
            self.server
                .auth_token
                .as_ref()
                .map(|token| token.trim().to_string())
                .filter(|token| !token.is_empty())
        })
    }

    /// Resolve the OpenAI API key: env (`OPENAI_API_KEY`) overrides config.
    pub fn resolved_openai_api_key(&self) -> Option<String> {
        non_empty_env(ENV_OPENAI_API_KEY)
            .or_else(|| self.providers.openai.api_key.clone().filter(|key| !key.trim().is_empty()))
    }

    /// Resolve the Google API key: env (`GOOGLE_API_KEY`) overrides config.
    pub fn resolved_google_api_key(&self) -> Option<String> {
        non_empty_env(ENV_GOOGLE_API_KEY)
            .or_else(|| self.providers.google.api_key.clone().filter(|key| !key.trim().is_empty()))
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    env::var(key).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

/// Resolve the active config path using the documented precedence.
pub fn resolve_config_path() -> Result<PathBuf, ConfigError> {
    resolve_config_path_with(|key| env::var_os(key), env::var_os("HOME").map(PathBuf::from))
}

fn resolve_config_path_with<F>(
    lookup_var: F,
    home_dir: Option<PathBuf>,
) -> Result<PathBuf, ConfigError>
where
    F: Fn(&str) -> Option<OsString>,
{
    if let Some(path) = lookup_var(ENV_CONFIG_PATH).and_then(non_empty_os_string) {
        return Ok(PathBuf::from(path));
    }

    if let Some(xdg_config_home) = lookup_var("XDG_CONFIG_HOME").and_then(non_empty_os_string) {
        return Ok(PathBuf::from(xdg_config_home)
            .join(DEFAULT_CONFIG_DIR_NAME)
            .join(DEFAULT_CONFIG_FILE_NAME));
    }

    let home = home_dir.ok_or(ConfigError::HomeDirectoryNotSet)?;
    Ok(home.join(".config").join(DEFAULT_CONFIG_DIR_NAME).join(DEFAULT_CONFIG_FILE_NAME))
}

fn non_empty_os_string(value: OsString) -> Option<OsString> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn write_default_config(path: &Path) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ConfigError::CreateConfigDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let rendered = toml::to_string_pretty(&AppConfig::launchable_default())
        .map_err(|source| ConfigError::SerializeDefaultConfig { source })?;
    fs::write(path, rendered)
        .map_err(|source| ConfigError::WriteDefaultConfig { path: path.to_path_buf(), source })
}

/// Errors raised while resolving, reading, or parsing configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("unable to resolve config path because HOME is not set")]
    HomeDirectoryNotSet,
    #[error("config file not found at expected path: {path}")]
    NotFound { path: PathBuf },
    #[error("failed to read config file at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config TOML at {path}: {source}")]
    ParseTomlAtPath {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to parse config TOML: {source}")]
    ParseToml {
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to create config directory {path}: {source}")]
    CreateConfigDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize default config: {source}")]
    SerializeDefaultConfig {
        #[source]
        source: toml::ser::Error,
    },
    #[error("failed to write default config to {path}: {source}")]
    WriteDefaultConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::{resolve_config_path_with, AppConfig, ConfigError, ProviderKind};
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn launchable_default_roundtrips_through_toml() {
        let config = AppConfig::launchable_default();
        let rendered = toml::to_string_pretty(&config).expect("serialize default");
        let parsed = AppConfig::from_toml_str(&rendered).expect("parse default");
        assert_eq!(config, parsed);
    }

    #[test]
    fn default_server_uses_enki_port_on_loopback() {
        let config = AppConfig::default();
        assert_eq!(config.server.port, crate::DEFAULT_PORT);
        assert_eq!(config.server.host, crate::DEFAULT_BIND_HOST);
        assert_eq!(config.tts.providers[0], ProviderKind::Apple);
    }

    #[test]
    fn rejects_unknown_fields() {
        let error =
            AppConfig::from_toml_str("[app]\nbogus = true\n").expect_err("unknown field must fail");
        assert!(matches!(error, ConfigError::ParseToml { .. }));
    }

    #[test]
    fn resolve_config_path_uses_expected_precedence() {
        let from_env = resolve_config_path_with(
            |name| match name {
                "ISIMUD_CONFIG" => Some(OsString::from("/tmp/override.toml")),
                "XDG_CONFIG_HOME" => Some(OsString::from("/xdg")),
                _ => None,
            },
            Some(PathBuf::from("/Users/alice")),
        )
        .expect("env override should resolve");
        assert_eq!(from_env, PathBuf::from("/tmp/override.toml"));

        let from_xdg = resolve_config_path_with(
            |name| match name {
                "XDG_CONFIG_HOME" => Some(OsString::from("/xdg")),
                _ => None,
            },
            Some(PathBuf::from("/Users/alice")),
        )
        .expect("xdg should resolve");
        assert_eq!(from_xdg, PathBuf::from("/xdg/isimud/config.toml"));

        let from_home = resolve_config_path_with(|_| None, Some(PathBuf::from("/Users/alice")))
            .expect("home should resolve");
        assert_eq!(from_home, PathBuf::from("/Users/alice/.config/isimud/config.toml"));
    }

    #[test]
    fn parses_named_voice_with_provider() {
        let config = AppConfig::from_toml_str(
            "[voices.narrator]\nprovider = \"openai\"\nvoice = \"onyx\"\n",
        )
        .expect("voice config should parse");
        let voice = config.voices.get("narrator").expect("narrator present");
        assert_eq!(voice.provider, ProviderKind::OpenAi);
        assert_eq!(voice.voice.as_deref(), Some("onyx"));
    }
}
