//! isimud binary entry point.
//!
//! Loads configuration, initializes logging, synchronizes macOS autostart, and hands off to
//! the runtime shell (menu bar + MCP server, or headless MCP server).

mod autostart;
mod logging;
mod runtime_shell;
mod runtime_tray;

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use isimud::config::{resolve_config_path, AppConfig};
use isimud::{TARGET_CONFIG, TARGET_RUNTIME};
use tracing::{info, warn};

fn main() -> ExitCode {
    maybe_load_dotenv();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("isimud {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let headless = args.iter().any(|arg| arg == "--headless");

    match bootstrap(headless) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("isimud failed to start: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn bootstrap(headless: bool) -> Result<()> {
    let config_path = resolve_config_path().context("resolving configured AppConfig path")?;
    let config = AppConfig::load().context("loading AppConfig from configured path")?;
    logging::init_logging(&config)?;
    sync_os_autostart(&config_path, &config);

    info!(
        target: TARGET_RUNTIME,
        headless,
        menubar = config.app.menubar,
        port = config.server.port,
        "loaded application configuration"
    );

    runtime_shell::run(config, headless)
}

fn sync_os_autostart(config_path: &Path, config: &AppConfig) {
    match autostart::sync_autostart(config_path, config) {
        Ok(autostart::AutostartSyncStatus::Enabled { plist_path, launch_path, changed }) => info!(
            target: TARGET_CONFIG,
            plist_path = %plist_path.display(),
            launch_path = %launch_path.display(),
            changed,
            "synced macOS autostart launch agent"
        ),
        Ok(autostart::AutostartSyncStatus::Disabled { plist_path, removed }) => info!(
            target: TARGET_CONFIG,
            plist_path = %plist_path.display(),
            removed,
            "disabled macOS autostart launch agent"
        ),
        Ok(autostart::AutostartSyncStatus::Unsupported) => {}
        Err(error) => warn!(
            target: TARGET_CONFIG,
            %error,
            "failed to sync macOS autostart"
        ),
    }
}

fn maybe_load_dotenv() {
    let disabled = std::env::var("ISIMUD_LOAD_DOTENV")
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"))
        .unwrap_or(false);
    if disabled {
        return;
    }
    let _ = dotenvy::dotenv();
}

fn print_help() {
    println!(
        "isimud {}\n\nUsage: isimud [OPTIONS]\n\n\
         Options:\n  \
         --headless     Run only the MCP server (no menu bar tray)\n  \
         -h, --help     Print this help\n  \
         -V, --version  Print the version\n\n\
         Configuration is loaded from ISIMUD_CONFIG, then $XDG_CONFIG_HOME/isimud/config.toml,\n\
         then ~/.config/isimud/config.toml (created with defaults if absent).",
        env!("CARGO_PKG_VERSION")
    );
}
