//! Runtime shell wiring (PLAN.md task 8).
//!
//! Builds the tokio runtime, starts the speech worker and MCP/HTTP server, and runs the `tao`
//! event loop for the tray. Supports `--headless` / `[app].menubar = false` (no tray).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use isimud::config::AppConfig;
use isimud::server::{self, ServerError};
use isimud::worker::SpeechEngine;
use isimud::TARGET_RUNTIME;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
#[cfg(target_os = "macos")]
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};

use crate::config_watch::{spawn_config_watcher, ConfigReloadResult};
use crate::runtime_tray::{
    build_tray, install_tray_event_bridge, map_tray_event, send_user_event, IndicatorState,
    TrayColors, UserEvent,
};
use isimud::state::SpeechEvent;
use isimud::voices::SpeakRequest;

/// Interval driving the speaking pulse animation.
const PULSE_INTERVAL: Duration = Duration::from_millis(350);

/// Build the runtime, start the worker + MCP server, and run either the tray event loop or a
/// headless wait-for-signal loop.
pub fn run(config: AppConfig, config_path: PathBuf, headless: bool) -> Result<()> {
    let config = Arc::new(config);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    let engine = SpeechEngine::new(config.clone());
    let (shutdown_tx, _shutdown_rx) = broadcast::channel::<()>(8);

    let guard = runtime.enter();
    let worker = engine.start();
    let supervisor_engine = engine.clone();
    let supervisor: JoinHandle<()> = runtime.spawn(async move {
        match worker.await {
            Ok(()) if supervisor_engine.is_shutdown() => {
                info!(target: TARGET_RUNTIME, "speech worker stopped during shutdown");
            }
            Ok(()) => {
                error!(target: TARGET_RUNTIME, "speech worker exited unexpectedly");
                supervisor_engine.mark_degraded("speech worker exited unexpectedly");
            }
            Err(error) => {
                error!(target: TARGET_RUNTIME, %error, "speech worker task join error");
                supervisor_engine.mark_degraded(format!("speech worker task join error: {error}"));
            }
        }
    });
    let server_engine = engine.clone();
    let server_shutdown = shutdown_tx.clone();
    let server_handle: JoinHandle<Result<(), ServerError>> =
        runtime.spawn(async move { server::run_server(server_engine, server_shutdown).await });
    drop(guard);

    let menubar = config.app.menubar && !headless;
    if !menubar {
        return run_headless(
            &runtime,
            engine,
            config_path,
            server_handle,
            supervisor,
            &shutdown_tx,
        );
    }
    run_tray(runtime, engine, config_path, shutdown_tx, server_handle, supervisor)
}

/// Headless mode: serve until Ctrl-C, then shut the server down gracefully. The config watcher
/// reloads the engine in place (no tray to repaint).
fn run_headless(
    runtime: &tokio::runtime::Runtime,
    engine: SpeechEngine,
    config_path: PathBuf,
    server_handle: JoinHandle<Result<(), ServerError>>,
    supervisor: JoinHandle<()>,
    shutdown_tx: &broadcast::Sender<()>,
) -> Result<()> {
    info!(target: TARGET_RUNTIME, "running headless (menu bar disabled)");
    spawn_config_watcher(config_path, move |result| match result {
        ConfigReloadResult::Loaded(config) => {
            engine.reload_config(Arc::new(*config));
            info!(target: TARGET_RUNTIME, "configuration reloaded");
        }
        ConfigReloadResult::Failed(reason) => {
            warn!(target: TARGET_RUNTIME, %reason, "config reload failed; keeping previous configuration");
        }
    });
    runtime.block_on(async move {
        if let Err(error) = tokio::signal::ctrl_c().await {
            error!(target: TARGET_RUNTIME, %error, "failed to listen for Ctrl-C");
        }
        info!(target: TARGET_RUNTIME, "shutdown signal received");
        let _ = shutdown_tx.send(());
        match server_handle.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => error!(target: TARGET_RUNTIME, %error, "MCP server error"),
            Err(error) => error!(target: TARGET_RUNTIME, %error, "MCP server task join error"),
        }
        supervisor.abort();
    });
    Ok(())
}

/// Tray mode: run the `tao` event loop, animate the indicator, and bridge engine events.
fn run_tray(
    runtime: tokio::runtime::Runtime,
    engine: SpeechEngine,
    config_path: PathBuf,
    shutdown_tx: broadcast::Sender<()>,
    server_handle: JoinHandle<Result<(), ServerError>>,
    supervisor: JoinHandle<()>,
) -> Result<()> {
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let proxy = event_loop.create_proxy();
    let handle = runtime.handle().clone();

    // Bridge tray-icon clicks into the event loop (a left click speaks a fortune).
    install_tray_event_bridge(proxy.clone());

    // Register the isimud:// URL scheme handler before the event loop runs. macOS delivers the
    // launch GetURL Apple Event during applicationWillFinishLaunching, earlier than
    // StartCause::Init, so the observer installed here must already be in place to catch URLs
    // that cold-launch the app.
    #[cfg(target_os = "macos")]
    crate::url_scheme::install_url_scheme_handler(proxy.clone());

    // Bridge config-file changes into the event loop so the tray is repainted on the main thread.
    let watch_proxy = proxy.clone();
    spawn_config_watcher(config_path, move |result| {
        let (event, context) = match result {
            ConfigReloadResult::Loaded(config) => {
                (UserEvent::ConfigReloaded(config), "config_reload_success")
            }
            ConfigReloadResult::Failed(reason) => {
                (UserEvent::ConfigReloadFailed(reason), "config_reload_failed")
            }
        };
        send_user_event(&watch_proxy, event, context);
    });

    let mut colors = TrayColors::from_config(&engine.config().indicator.colors);
    let mut tray: Option<crate::runtime_tray::Tray> = None;
    let mut indicator = IndicatorState::Idle;
    let mut pulse_on = true;
    let mut degraded = false;
    let mut server_handle = Some(server_handle);

    info!(target: TARGET_RUNTIME, "starting menu bar event loop");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                match build_tray(colors) {
                    Ok(built) => {
                        built.update(IndicatorState::Idle, true, degraded);
                        tray = Some(built);
                    }
                    Err(error) => {
                        error!(target: TARGET_RUNTIME, %error, "failed to build tray; exiting");
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                }

                let mut events = engine.subscribe();
                let speech_proxy = proxy.clone();
                handle.spawn(async move {
                    loop {
                        match events.recv().await {
                            Ok(event) => {
                                if speech_proxy.send_event(UserEvent::Speech(event)).is_err() {
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });

                let tick_proxy = proxy.clone();
                handle.spawn(async move {
                    let mut interval = tokio::time::interval(PULSE_INTERVAL);
                    loop {
                        interval.tick().await;
                        if tick_proxy.send_event(UserEvent::Tick).is_err() {
                            break;
                        }
                    }
                });

                if let Some(server_handle) = server_handle.take() {
                    let server_proxy = proxy.clone();
                    handle.spawn(async move {
                        let message = match server_handle.await {
                            Ok(Ok(())) => None,
                            Ok(Err(error)) => Some(error.to_string()),
                            Err(error) => Some(error.to_string()),
                        };
                        let _ = server_proxy.send_event(UserEvent::ServerStopped(message));
                    });
                }
            }
            Event::UserEvent(UserEvent::Speech(event)) => {
                if let SpeechEvent::Degraded { .. } = event {
                    degraded = true;
                    if let Some(tray) = tray.as_ref() {
                        tray.update(indicator, pulse_on, degraded);
                    }
                    return;
                }
                let next = match event {
                    SpeechEvent::Started { .. } => Some(IndicatorState::Speaking),
                    SpeechEvent::Finished { .. }
                    | SpeechEvent::Failed { .. }
                    | SpeechEvent::Stopped { .. } => Some(IndicatorState::Idle),
                    SpeechEvent::Enqueued { .. } | SpeechEvent::Degraded { .. } => None,
                };
                if let Some(next) = next {
                    if next != indicator {
                        indicator = next;
                        pulse_on = true;
                        if let Some(tray) = tray.as_ref() {
                            tray.update(indicator, pulse_on, degraded);
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::Tick) if indicator == IndicatorState::Speaking => {
                pulse_on = !pulse_on;
                if let Some(tray) = tray.as_ref() {
                    tray.update(indicator, pulse_on, degraded);
                }
            }
            Event::UserEvent(UserEvent::Speak(request)) => match engine.enqueue(request) {
                Ok(job_id) => {
                    info!(target: TARGET_RUNTIME, %job_id, "enqueued speech from isimud:// URL")
                }
                Err(error) => {
                    warn!(target: TARGET_RUNTIME, ?error, "failed to enqueue speech from isimud:// URL")
                }
            },
            Event::UserEvent(UserEvent::TrayEvent(event)) if map_tray_event(&event) => {
                let fortune_engine = engine.clone();
                handle.spawn(async move {
                    match run_fortune().await {
                        Ok(text) => {
                            let request = SpeakRequest { text, voice: None, rate: None };
                            match fortune_engine.enqueue(request) {
                                Ok(job_id) => {
                                    info!(target: TARGET_RUNTIME, %job_id, "enqueued fortune from tray click")
                                }
                                Err(error) => {
                                    warn!(target: TARGET_RUNTIME, ?error, "failed to enqueue fortune from tray click")
                                }
                            }
                        }
                        Err(error) => {
                            warn!(target: TARGET_RUNTIME, %error, "fortune unavailable; tray click ignored")
                        }
                    }
                });
            }
            Event::UserEvent(UserEvent::ServerStopped(message)) => {
                match &message {
                    Some(error) => {
                        error!(target: TARGET_RUNTIME, %error, "MCP server stopped; exiting")
                    }
                    None => info!(target: TARGET_RUNTIME, "MCP server stopped; exiting"),
                }
                let _ = shutdown_tx.send(());
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::ConfigReloaded(new_config)) => {
                let new_config = Arc::new(*new_config);
                engine.reload_config(new_config.clone());
                colors = TrayColors::from_config(&new_config.indicator.colors);
                if let Some(tray) = tray.as_mut() {
                    tray.set_colors(colors);
                    tray.update(indicator, pulse_on, degraded);
                }
                info!(target: TARGET_RUNTIME, "configuration reloaded");
            }
            Event::UserEvent(UserEvent::ConfigReloadFailed(reason)) => {
                warn!(target: TARGET_RUNTIME, %reason, "config reload failed; keeping previous configuration");
            }
            _ => {}
        }

        // Keep the runtime and engine alive for the lifetime of the event loop.
        let _ = &runtime;
        let _ = &engine;
        let _ = &supervisor;
    })
}

/// Run the `fortune` binary and return its trimmed output. Errors if `fortune` is not installed
/// or produces no output, so a tray click is silently ignored when fortune is unavailable.
async fn run_fortune() -> Result<String> {
    let output = tokio::process::Command::new("fortune")
        .output()
        .await
        .context("spawning fortune (is it installed?)")?;
    if !output.status.success() {
        anyhow::bail!("fortune exited with status {}", output.status);
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        anyhow::bail!("fortune produced no output");
    }
    Ok(text)
}
