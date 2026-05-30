//! Logging initialization (PLAN.md task 2).
//!
//! stderr `fmt` layer plus per-target `oslog` layers on macOS, mirroring MUNINN. Honors
//! `RUST_LOG` with isimud's tracing targets.

use anyhow::{anyhow, Result};
use isimud::AppConfig;
use isimud::{
    TARGET_CONFIG, TARGET_DEFAULT, TARGET_PROVIDER, TARGET_RUNTIME, TARGET_SERVER, TARGET_SPEECH,
};
use tracing::info;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

#[cfg(target_os = "macos")]
use tracing_oslog::OsLogger;
#[cfg(target_os = "macos")]
use tracing_subscriber::filter::filter_fn;

#[cfg(target_os = "macos")]
const OSLOG_SUBSYSTEM: &str = "com.bnomei.isimud";

/// Initialize the global tracing subscriber: a stderr `fmt` layer honoring `RUST_LOG`
/// (falling back to `[logging].level`), plus per-target `oslog` layers on macOS.
pub fn init_logging(config: &AppConfig) -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(config.logging.level.clone()));
    let fmt_layer =
        tracing_subscriber::fmt::layer().with_target(true).with_writer(std::io::stderr).compact();

    let subscriber = tracing_subscriber::registry().with(env_filter).with(fmt_layer);

    #[cfg(target_os = "macos")]
    let subscriber = subscriber
        .with(
            OsLogger::new(OSLOG_SUBSYSTEM, TARGET_RUNTIME)
                .with_filter(filter_fn(|metadata| metadata.target() == TARGET_RUNTIME)),
        )
        .with(
            OsLogger::new(OSLOG_SUBSYSTEM, TARGET_SERVER)
                .with_filter(filter_fn(|metadata| metadata.target() == TARGET_SERVER)),
        )
        .with(
            OsLogger::new(OSLOG_SUBSYSTEM, TARGET_PROVIDER)
                .with_filter(filter_fn(|metadata| metadata.target() == TARGET_PROVIDER)),
        )
        .with(
            OsLogger::new(OSLOG_SUBSYSTEM, TARGET_CONFIG)
                .with_filter(filter_fn(|metadata| metadata.target() == TARGET_CONFIG)),
        )
        .with(
            OsLogger::new(OSLOG_SUBSYSTEM, TARGET_SPEECH)
                .with_filter(filter_fn(|metadata| metadata.target() == TARGET_SPEECH)),
        )
        .with(OsLogger::new(OSLOG_SUBSYSTEM, TARGET_DEFAULT).with_filter(filter_fn(|metadata| {
            let target = metadata.target();
            target != TARGET_RUNTIME
                && target != TARGET_SERVER
                && target != TARGET_PROVIDER
                && target != TARGET_CONFIG
                && target != TARGET_SPEECH
        })));

    subscriber.try_init().map_err(|error| anyhow!("initializing tracing subscriber: {error}"))?;

    info!(
        target: TARGET_RUNTIME,
        level = %config.logging.level,
        "logging initialized"
    );

    Ok(())
}
