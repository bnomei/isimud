//! Runtime shell wiring (PLAN.md task 8).
//!
//! Builds the tokio runtime, starts the speech worker and MCP/HTTP server, and runs the `tao`
//! event loop for the tray. Supports `--headless` / `[app].menubar = false` (no tray).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use isimud::config::AppConfig;
use isimud::server::{self, ServerError};
use isimud::worker::SpeechEngine;
use isimud::TARGET_RUNTIME;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{error, info};

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
#[cfg(target_os = "macos")]
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::MenuEvent;

use crate::runtime_tray::{build_tray, IndicatorState, UserEvent};
use isimud::state::SpeechEvent;

/// Interval driving the speaking pulse animation.
const PULSE_INTERVAL: Duration = Duration::from_millis(350);

/// Build the runtime, start the worker + MCP server, and run either the tray event loop or a
/// headless wait-for-signal loop.
pub fn run(config: AppConfig, headless: bool) -> Result<()> {
    let config = Arc::new(config);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    let engine = SpeechEngine::new(config.clone());
    let (shutdown_tx, _shutdown_rx) = broadcast::channel::<()>(8);

    let guard = runtime.enter();
    let _worker = engine.start();
    let server_engine = engine.clone();
    let server_shutdown = shutdown_tx.clone();
    let server_handle: JoinHandle<Result<(), ServerError>> =
        runtime.spawn(async move { server::run_server(server_engine, server_shutdown).await });
    drop(guard);

    let menubar = config.app.menubar && !headless;
    if !menubar {
        return run_headless(&runtime, server_handle, &shutdown_tx);
    }
    run_tray(runtime, engine, shutdown_tx, server_handle)
}

/// Headless mode: serve until Ctrl-C, then shut the server down gracefully.
fn run_headless(
    runtime: &tokio::runtime::Runtime,
    server_handle: JoinHandle<Result<(), ServerError>>,
    shutdown_tx: &broadcast::Sender<()>,
) -> Result<()> {
    info!(target: TARGET_RUNTIME, "running headless (menu bar disabled)");
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
    });
    Ok(())
}

/// Tray mode: run the `tao` event loop, animate the indicator, and bridge engine events.
fn run_tray(
    runtime: tokio::runtime::Runtime,
    engine: SpeechEngine,
    shutdown_tx: broadcast::Sender<()>,
    server_handle: JoinHandle<Result<(), ServerError>>,
) -> Result<()> {
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let proxy = event_loop.create_proxy();
    let handle = runtime.handle().clone();

    let mut tray: Option<crate::runtime_tray::Tray> = None;
    let mut indicator = IndicatorState::Idle;
    let mut pulse_on = true;
    let mut server_handle = Some(server_handle);

    info!(target: TARGET_RUNTIME, "starting menu bar event loop");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                match build_tray() {
                    Ok(built) => {
                        let quit_id = built.quit_id().clone();
                        let menu_proxy = proxy.clone();
                        MenuEvent::set_event_handler(Some(move |menu_event: MenuEvent| {
                            if menu_event.id == quit_id {
                                let _ = menu_proxy.send_event(UserEvent::Quit);
                            }
                        }));
                        built.update(IndicatorState::Idle, true);
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
                let next = match event {
                    SpeechEvent::Started { .. } => Some(IndicatorState::Speaking),
                    SpeechEvent::Finished { .. }
                    | SpeechEvent::Failed { .. }
                    | SpeechEvent::Stopped { .. } => Some(IndicatorState::Idle),
                    SpeechEvent::Enqueued { .. } => None,
                };
                if let Some(next) = next {
                    if next != indicator {
                        indicator = next;
                        pulse_on = true;
                        if let Some(tray) = tray.as_ref() {
                            tray.update(indicator, pulse_on);
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::Tick) if indicator == IndicatorState::Speaking => {
                pulse_on = !pulse_on;
                if let Some(tray) = tray.as_ref() {
                    tray.update(indicator, pulse_on);
                }
            }
            Event::UserEvent(UserEvent::Quit) => {
                info!(target: TARGET_RUNTIME, "quit requested from tray");
                let _ = shutdown_tx.send(());
                *control_flow = ControlFlow::Exit;
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
            _ => {}
        }

        // Keep the runtime and engine alive for the lifetime of the event loop.
        let _ = &runtime;
        let _ = &engine;
    })
}
