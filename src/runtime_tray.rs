//! Menu bar tray icon and pulse animation (PLAN.md task 8).
//!
//! Renders the `tray-icon` glyph icon and pulses it while isimud is speaking, idle otherwise,
//! mirroring MUNINN's indicator renderer.

use anyhow::{Context, Result};
use isimud::state::SpeechEvent;
use tray_icon::menu::{Menu, MenuId, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// Side length of the rendered menu-bar icon, in pixels.
const ICON_SIZE: u32 = 36;

/// Events delivered into the `tao` event loop from background tasks and the tray menu.
#[derive(Debug, Clone)]
pub enum UserEvent {
    /// A speech lifecycle event from the engine.
    Speech(SpeechEvent),
    /// Periodic tick driving the pulse animation while speaking.
    Tick,
    /// The user chose "Quit" from the tray menu.
    Quit,
    /// The MCP server task ended (carrying an optional error message).
    ServerStopped(Option<String>),
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
    quit_id: MenuId,
    _menu: Menu,
    _quit_item: MenuItem,
}

impl Tray {
    /// The menu id of the "Quit" item, used to match incoming menu events.
    pub fn quit_id(&self) -> &MenuId {
        &self.quit_id
    }

    /// Update the icon and tooltip for the given state and pulse phase.
    pub fn update(&self, state: IndicatorState, pulse_on: bool) {
        let icon = match state {
            IndicatorState::Idle => indicator_icon(IDLE_COLOR),
            IndicatorState::Speaking => {
                indicator_icon(if pulse_on { SPEAKING_BRIGHT } else { SPEAKING_DIM })
            }
        };
        if let Ok(icon) = icon {
            if let Err(error) = self.icon.set_icon(Some(icon)) {
                tracing::warn!(target: isimud::TARGET_RUNTIME, %error, "failed to set tray icon");
            }
        }
        let tooltip = match state {
            IndicatorState::Idle => "isimud — idle",
            IndicatorState::Speaking => "isimud — speaking",
        };
        let _ = self.icon.set_tooltip(Some(tooltip));
    }
}

const IDLE_COLOR: (u8, u8, u8) = (96, 104, 120);
const SPEAKING_BRIGHT: (u8, u8, u8) = (72, 199, 116);
const SPEAKING_DIM: (u8, u8, u8) = (48, 132, 78);

/// Build the tray icon with a "Quit" menu, starting in the idle appearance.
pub fn build_tray() -> Result<Tray> {
    let menu = Menu::new();
    let quit_item = MenuItem::new("Quit isimud", true, None);
    menu.append(&quit_item).context("appending tray Quit item")?;
    let quit_id = quit_item.id().clone();

    let icon = indicator_icon(IDLE_COLOR).context("building initial tray icon")?;
    let tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("isimud — idle")
        .with_menu(Box::new(menu.clone()))
        .build()
        .context("creating menu bar tray icon")?;

    Ok(Tray { icon: tray, quit_id, _menu: menu, _quit_item: quit_item })
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
