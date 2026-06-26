//! Hot-reload watcher for the configuration file.
//!
//! A background thread fingerprints the config path (mtime + length) and snapshots contents,
//! polling with 250ms→2s backoff. On a real content change it parses and validates via
//! [`AppConfig::from_toml_str`] and delivers a [`ConfigReloadResult`] to the caller (tray
//! event loop in menubar mode, direct engine reload when headless).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use isimud::config::AppConfig;
use isimud::TARGET_CONFIG;
use tracing::info;

const POLL_MIN_INTERVAL: Duration = Duration::from_millis(250);
const POLL_MAX_INTERVAL: Duration = Duration::from_secs(2);

/// Outcome of a config file change: either a parsed+validated config or a human-readable error.
pub enum ConfigReloadResult {
    Loaded(Box<AppConfig>),
    Failed(String),
}

/// Spawn the background thread that watches `config_path`, invoking `on_change` on each real change.
pub fn spawn_config_watcher<F>(config_path: PathBuf, on_change: F)
where
    F: Fn(ConfigReloadResult) + Send + 'static,
{
    std::thread::spawn(move || {
        info!(target: TARGET_CONFIG, path = %config_path.display(), "config watcher started");
        let mut last_fingerprint = read_config_fingerprint(&config_path);
        let mut last_snapshot = read_config_snapshot(&config_path);
        let mut poll_interval = POLL_MIN_INTERVAL;

        loop {
            std::thread::sleep(poll_interval);

            let fingerprint = read_config_fingerprint(&config_path);
            if fingerprint == last_fingerprint {
                poll_interval = next_poll_interval(poll_interval, false);
                continue;
            }

            let snapshot = read_config_snapshot(&config_path);
            if snapshot == last_snapshot {
                last_fingerprint = fingerprint;
                poll_interval = next_poll_interval(poll_interval, false);
                continue;
            }

            info!(target: TARGET_CONFIG, path = %config_path.display(), "config file changed; reloading");
            on_change(evaluate_change(&snapshot, &config_path));

            last_fingerprint = fingerprint;
            last_snapshot = snapshot;
            poll_interval = POLL_MIN_INTERVAL;
        }
    });
}

fn evaluate_change(snapshot: &ConfigSnapshot, config_path: &Path) -> ConfigReloadResult {
    match snapshot {
        ConfigSnapshot::Contents(contents) => match AppConfig::from_toml_str(contents) {
            Ok(config) => ConfigReloadResult::Loaded(Box::new(config)),
            Err(error) => ConfigReloadResult::Failed(format!("{}: {error}", config_path.display())),
        },
        ConfigSnapshot::Missing => {
            ConfigReloadResult::Failed(format!("{}: config file missing", config_path.display()))
        }
        ConfigSnapshot::Unreadable(error) => {
            ConfigReloadResult::Failed(format!("{}: {error}", config_path.display()))
        }
    }
}

/// Double the poll interval (up to the maximum) until a change is observed, then reset to minimum.
fn next_poll_interval(current: Duration, observed_change: bool) -> Duration {
    if observed_change {
        return POLL_MIN_INTERVAL;
    }
    let doubled_ms = current.as_millis().saturating_mul(2);
    let capped_ms = doubled_ms.min(POLL_MAX_INTERVAL.as_millis());
    Duration::from_millis(capped_ms as u64)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigFingerprint {
    Missing,
    Metadata { modified_at: Option<SystemTime>, len: u64 },
    Unreadable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigSnapshot {
    Missing,
    Contents(String),
    Unreadable(String),
}

fn read_config_fingerprint(path: &Path) -> ConfigFingerprint {
    match fs::metadata(path) {
        Ok(metadata) => ConfigFingerprint::Metadata {
            modified_at: metadata.modified().ok(),
            len: metadata.len(),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ConfigFingerprint::Missing,
        Err(error) => ConfigFingerprint::Unreadable(error.to_string()),
    }
}

fn read_config_snapshot(path: &Path) -> ConfigSnapshot {
    match fs::read_to_string(path) {
        Ok(contents) => ConfigSnapshot::Contents(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ConfigSnapshot::Missing,
        Err(error) => ConfigSnapshot::Unreadable(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_poll_interval_doubles_until_maximum() {
        assert_eq!(next_poll_interval(POLL_MIN_INTERVAL, false), Duration::from_millis(500));
        assert_eq!(next_poll_interval(Duration::from_secs(1), false), POLL_MAX_INTERVAL);
        assert_eq!(next_poll_interval(POLL_MAX_INTERVAL, false), POLL_MAX_INTERVAL);
    }

    #[test]
    fn next_poll_interval_resets_to_minimum_after_change() {
        assert_eq!(next_poll_interval(POLL_MAX_INTERVAL, true), POLL_MIN_INTERVAL);
    }
}
