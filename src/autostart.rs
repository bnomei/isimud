//! macOS LaunchAgent autostart sync (PLAN.md task 8).
//!
//! Installs/removes a per-user LaunchAgent plist so isimud can start at login, mirroring
//! MUNINN's autostart behavior. Driven by `[app].autostart`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use isimud::AppConfig;

#[cfg(target_os = "macos")]
const LAUNCH_AGENT_LABEL: &str = "com.bnomei.isimud";
#[cfg(target_os = "macos")]
const LAUNCH_AGENT_FILE_NAME: &str = "com.bnomei.isimud.plist";
#[cfg(target_os = "macos")]
const DEFAULT_LAUNCH_AGENT_PATH: &str =
    "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// Outcome of synchronizing the autostart LaunchAgent with configuration.
#[derive(Debug)]
pub enum AutostartSyncStatus {
    /// Autostart is enabled; the plist is installed (`changed` if it was (re)written).
    Enabled { plist_path: PathBuf, launch_path: PathBuf, changed: bool },
    /// Autostart is disabled; the plist was removed if it existed.
    Disabled { plist_path: PathBuf, removed: bool },
    /// This platform does not support autostart.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Unsupported,
}

#[cfg(target_os = "macos")]
pub fn sync_autostart(config_path: &Path, config: &AppConfig) -> Result<AutostartSyncStatus> {
    use anyhow::Context;

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("resolving HOME for macOS autostart")?;
    let plist_path = home.join("Library/LaunchAgents").join(LAUNCH_AGENT_FILE_NAME);

    if !config.app.autostart {
        let removed = if plist_path.exists() {
            std::fs::remove_file(&plist_path)
                .with_context(|| format!("removing autostart plist {}", plist_path.display()))?;
            true
        } else {
            false
        };
        return Ok(AutostartSyncStatus::Disabled { plist_path, removed });
    }

    let launch_path = std::env::current_exe().context("resolving current executable path")?;
    let canonical_config =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    let working_directory =
        canonical_config.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"));

    let rendered = render_plist(&launch_path, &canonical_config, &working_directory);
    let changed = write_if_changed(&plist_path, &rendered)?;

    Ok(AutostartSyncStatus::Enabled { plist_path, launch_path, changed })
}

#[cfg(not(target_os = "macos"))]
pub fn sync_autostart(_config_path: &Path, _config: &AppConfig) -> Result<AutostartSyncStatus> {
    Ok(AutostartSyncStatus::Unsupported)
}

#[cfg(target_os = "macos")]
fn write_if_changed(path: &Path, contents: &str) -> Result<bool> {
    use anyhow::Context;

    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing == contents {
            return Ok(false);
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating LaunchAgents dir {}", parent.display()))?;
    }
    std::fs::write(path, contents)
        .with_context(|| format!("writing autostart plist {}", path.display()))?;
    Ok(true)
}

#[cfg(target_os = "macos")]
fn render_plist(launch_path: &Path, config_path: &Path, working_directory: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{launch_path}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <false/>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
  <key>WorkingDirectory</key>
  <string>{working_directory}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>ISIMUD_CONFIG</key>
    <string>{config_path}</string>
    <key>PATH</key>
    <string>{path}</string>
  </dict>
</dict>
</plist>
"#,
        label = LAUNCH_AGENT_LABEL,
        launch_path = escape_plist_xml(&launch_path.display().to_string()),
        working_directory = escape_plist_xml(&working_directory.display().to_string()),
        config_path = escape_plist_xml(&config_path.display().to_string()),
        path = DEFAULT_LAUNCH_AGENT_PATH,
    )
}

#[cfg(target_os = "macos")]
fn escape_plist_xml(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
