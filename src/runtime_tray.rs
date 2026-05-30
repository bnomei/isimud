//! Menu bar tray icon and pulse animation (PLAN.md task 8).
//!
//! Renders the `tray-icon` glyph icon and pulses it while isimud is speaking, idle otherwise,
//! mirroring MUNINN's indicator renderer.

use anyhow::{Context, Result};
use isimud::config::{AppConfig, IndicatorColorsConfig};
use isimud::state::SpeechEvent;
use isimud::voices::SpeakRequest;
use tao::event_loop::EventLoopProxy;
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Side length of the rendered menu-bar icon, in pixels.
const ICON_SIZE: u32 = 36;

/// Events delivered into the `tao` event loop from background tasks and the tray menu.
#[derive(Debug, Clone)]
pub enum UserEvent {
    /// A speech lifecycle event from the engine.
    Speech(SpeechEvent),
    /// A speech request parsed from an `isimud://speak` URL, to be enqueued.
    Speak(SpeakRequest),
    /// Periodic tick driving the pulse animation while speaking.
    Tick,
    /// A raw tray-icon mouse event (e.g. a left click to speak a fortune).
    TrayEvent(TrayIconEvent),
    /// The MCP server task ended (carrying an optional error message).
    ServerStopped(Option<String>),
    /// The config file changed and parsed/validated successfully; carries the new config.
    ConfigReloaded(Box<AppConfig>),
    /// The config file changed but could not be parsed or validated (carries the reason).
    ConfigReloadFailed(String),
}

/// Whether the indicator should render the idle or the active (speaking) appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicatorState {
    Idle,
    Speaking,
}

/// Live tray handle plus the menu items it owns (kept alive for the tray's lifetime).
pub struct Tray {
    icon: TrayIcon,
    colors: TrayColors,
}

impl Tray {
    /// Replace the active color palette (used when the config is hot-reloaded).
    pub fn set_colors(&mut self, colors: TrayColors) {
        self.colors = colors;
    }

    /// Update the icon and tooltip for the given state, pulse phase, and health.
    pub fn update(&self, state: IndicatorState, pulse_on: bool, degraded: bool) {
        let icon = match state {
            IndicatorState::Idle => indicator_icon(self.colors.idle),
            IndicatorState::Speaking => indicator_icon(if pulse_on {
                self.colors.speaking_bright
            } else {
                self.colors.speaking_dim
            }),
        };
        if let Ok(icon) = icon {
            if let Err(error) = self.icon.set_icon(Some(icon)) {
                tracing::warn!(target: isimud::TARGET_RUNTIME, %error, "failed to set tray icon");
            }
        }
        let tooltip = if degraded {
            "isimud — degraded (see logs)"
        } else {
            match state {
                IndicatorState::Idle => "isimud — idle",
                IndicatorState::Speaking => "isimud — speaking",
            }
        };
        let _ = self.icon.set_tooltip(Some(tooltip));
    }
}

/// The tray icon palette, resolved from `[indicator.colors]` (or built-in defaults).
#[derive(Debug, Clone, Copy)]
pub struct TrayColors {
    idle: (u8, u8, u8),
    speaking_bright: (u8, u8, u8),
    speaking_dim: (u8, u8, u8),
}

impl Default for TrayColors {
    fn default() -> Self {
        // Aligned with MUNINN's Apple system-color palette: idle systemGray (#636366) and the
        // active pulse derived from systemGreen (#30D158), dimmed to ~66% for the off phase.
        Self { idle: (99, 99, 102), speaking_bright: (48, 209, 88), speaking_dim: (32, 138, 58) }
    }
}

impl TrayColors {
    /// Resolve colors from config, falling back to the default for any field that fails to parse.
    /// Config validation already rejects bad hex on load, so this fallback is purely defensive.
    pub fn from_config(colors: &IndicatorColorsConfig) -> Self {
        let default = Self::default();
        Self {
            idle: resolve_color("indicator.colors.idle", &colors.idle, default.idle),
            speaking_bright: resolve_color(
                "indicator.colors.speaking_bright",
                &colors.speaking_bright,
                default.speaking_bright,
            ),
            speaking_dim: resolve_color(
                "indicator.colors.speaking_dim",
                &colors.speaking_dim,
                default.speaking_dim,
            ),
        }
    }
}

fn resolve_color(name: &str, value: &str, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    match parse_hex_rgb(value) {
        Ok(rgb) => rgb,
        Err(error) => {
            tracing::warn!(target: isimud::TARGET_RUNTIME, color = name, %error, "invalid tray color; using default");
            fallback
        }
    }
}

/// Parse a `#RRGGBB` hex string into an `(r, g, b)` triple.
fn parse_hex_rgb(value: &str) -> Result<(u8, u8, u8)> {
    let Some(hex) = value.strip_prefix('#') else {
        anyhow::bail!("indicator color must start with '#': {value}");
    };
    if hex.len() != 6 {
        anyhow::bail!("indicator color must be exactly 6 hex digits: {value}");
    }
    let parse = |start: usize| {
        u8::from_str_radix(&hex[start..start + 2], 16)
            .with_context(|| format!("indicator color contains invalid hex digits: {value}"))
    };
    Ok((parse(0)?, parse(2)?, parse(4)?))
}

/// Send a [`UserEvent`] into the event loop, warning if the loop has already exited.
pub fn send_user_event(
    proxy: &EventLoopProxy<UserEvent>,
    event: UserEvent,
    context: &'static str,
) -> bool {
    match proxy.send_event(event) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(target: isimud::TARGET_RUNTIME, context, %error, "failed to send user event");
            false
        }
    }
}

/// Forward tray-icon mouse events into the `tao` event loop as [`UserEvent::TrayEvent`].
pub fn install_tray_event_bridge(proxy: EventLoopProxy<UserEvent>) {
    TrayIconEvent::set_event_handler(Some(move |event| {
        send_user_event(&proxy, UserEvent::TrayEvent(event), "tray_event_bridge");
    }));
}

/// Whether a tray event is a completed left click. Acting on the button release (a full click)
/// keeps a single click from firing the action twice.
pub fn map_tray_event(event: &TrayIconEvent) -> bool {
    matches!(
        event,
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
    )
}

/// Build the tray icon (no menu), starting in the idle appearance. A left click is delivered as
/// a [`UserEvent::TrayEvent`] via [`install_tray_event_bridge`].
pub fn build_tray(colors: TrayColors) -> Result<Tray> {
    let icon = indicator_icon(colors.idle).context("building initial tray icon")?;
    let tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("isimud — idle")
        .build()
        .context("creating menu bar tray icon")?;

    Ok(Tray { icon: tray, colors })
}

/// Render a filled-circle indicator of the given color into an RGBA icon.
fn indicator_icon(rgb: (u8, u8, u8)) -> Result<Icon> {
    let size = ICON_SIZE as f32;
    let center = size / 2.0 - 0.5;
    let radius = size / 2.0 - 2.0;

    let mut data = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = ((y * ICON_SIZE + x) * 4) as usize;
            // Soft 1px edge for mild antialiasing.
            let alpha = if dist <= radius - 1.0 {
                255.0
            } else if dist <= radius {
                (radius - dist).clamp(0.0, 1.0) * 255.0
            } else {
                0.0
            };
            data[idx] = rgb.0;
            data[idx + 1] = rgb.1;
            data[idx + 2] = rgb.2;
            data[idx + 3] = alpha as u8;
        }
    }

    Icon::from_rgba(data, ICON_SIZE, ICON_SIZE).context("constructing RGBA icon")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_rgb_accepts_valid_colors() {
        assert_eq!(parse_hex_rgb("#112233").unwrap(), (17, 34, 51));
        assert_eq!(parse_hex_rgb("#30D158").unwrap(), (48, 209, 88));
    }

    #[test]
    fn parse_hex_rgb_rejects_invalid_colors() {
        assert!(parse_hex_rgb("112233").is_err());
        assert!(parse_hex_rgb("#11223").is_err());
        assert!(parse_hex_rgb("#11zz33").is_err());
    }

    #[test]
    fn from_config_falls_back_on_bad_hex() {
        let colors = IndicatorColorsConfig {
            idle: "bad".to_string(),
            speaking_bright: "#30D158".to_string(),
            speaking_dim: "#208A3A".to_string(),
        };
        let resolved = TrayColors::from_config(&colors);
        assert_eq!(resolved.idle, TrayColors::default().idle);
        assert_eq!(resolved.speaking_bright, (48, 209, 88));
    }
}
