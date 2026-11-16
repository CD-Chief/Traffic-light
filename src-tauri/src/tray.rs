//! Tray icon: coloured by the most urgent state across all sessions, with a
//! menu for show/hide, design, start-on-login and quit.

use std::sync::atomic::Ordering;

use tauri::image::Image;
use tauri::menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;
use traffic_light_core::{most_urgent, Session, State};

use crate::Visibility;

const TRAY_ID: &str = "main";
/// Keep in step with THEMES in ui/main.js.
const THEMES: &[(&str, &str)] = &[
    ("classic", "Classic"),
    ("glass", "Liquid glass"),
    ("signal", "Signal"),
    ("neon", "Neon"),
    ("pills", "Pills"),
    ("terminal", "Terminal"),
];

fn colour(state: Option<State>) -> [u8; 3] {
    match state {
        Some(State::NeedsYou) => [0xff, 0xb0, 0x20],
        Some(State::Error) => [0xff, 0x45, 0x3a],
        Some(State::Working) => [0x30, 0xd1, 0x58],
        Some(State::Done) => [0x9a, 0xa0, 0xa6],
        None => [0x5f, 0x63, 0x68],
    }
}

/// A filled, anti-aliased dot. Drawn at runtime so there are no per-state
/// image assets to ship. With no sessions it is a hollow ring.
fn dot(state: Option<State>) -> Image<'static> {
    const SIZE: u32 = 32;
    let [r, g, b] = colour(state);
    let centre = (SIZE as f32 - 1.0) / 2.0;
    let radius = SIZE as f32 * 0.40;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 - centre).powi(2) + (y as f32 - centre).powi(2)).sqrt();
            let mut alpha = (radius - d + 0.5).clamp(0.0, 1.0);
            if state.is_none() {
                alpha *= (d - (radius - 4.0) + 0.5).clamp(0.0, 1.0);
            }
            rgba.extend_from_slice(&[r, g, b, (alpha * 255.0) as u8]);
        }
    }
    Image::new_owned(rgba, SIZE, SIZE)
}

fn tooltip(sessions: &[Session]) -> String {
    if sessions.is_empty() {
        return "Traffic Light: no Claude Code sessions".into();
    }
    let parts: Vec<String> = [State::NeedsYou, State::Error, State::Working, State::Done]
        .into_iter()
        .filter_map(|state| {
            let n = sessions.iter().filter(|s| s.state == state).count();
            (n > 0).then(|| format!("{n} {}", state.name()))
        })
        .collect();
    format!("Claude Code: {}", parts.join(", "))
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let toggle = MenuItemBuilder::with_id("toggle", "Show / hide widget").build(app)?;
    let mut design = SubmenuBuilder::new(app, "Design");
    for (id, name) in THEMES {
        design = design.text(format!("theme:{id}"), *name);
    }
    let design = design.build()?;
    let autostart = CheckMenuItemBuilder::with_id("autostart", "Start on login")
        .checked(app.autolaunch().is_enabled().unwrap_or(false))
        .build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&toggle)
        .item(&design)
        .item(&autostart)
        .separator()
        .item(&quit)
        .build()?;

    let autostart_item = autostart.clone();
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(dot(None))
        .tooltip(tooltip(&[]))
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "toggle" => {
                let vis = app.state::<Visibility>();
                vis.wanted.fetch_xor(true, Ordering::Relaxed);
                crate::publish(app, &app.state::<std::sync::Arc<traffic_light_core::Shared>>().sessions());
            }
            "autostart" => {
                let launcher = app.autolaunch();
                let enabled = launcher.is_enabled().unwrap_or(false);
                let _ = if enabled { launcher.disable() } else { launcher.enable() };
                let _ = autostart_item.set_checked(launcher.is_enabled().unwrap_or(!enabled));
            }
            "quit" => app.exit(0),
            id => {
                if let Some(theme) = id.strip_prefix("theme:") {
                    let _ = app.emit("theme-changed", theme);
                }
            }
        })
        .build(app)?;
    Ok(())
}

pub fn update(app: &AppHandle, sessions: &[Session]) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_icon(Some(dot(most_urgent(sessions))));
        let _ = tray.set_tooltip(Some(tooltip(sessions)));
    }
}

pub fn show_error(app: &AppHandle, message: &str) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_icon(Some(dot(Some(State::Error))));
        let _ = tray.set_tooltip(Some(format!("Traffic Light: {message}")));
    }
}

/// Mitigation 3 in the spec: be listening from login so hooks never hit a
/// closed port. Done once, in release builds, so "Start on login" can be
/// unticked afterwards and stay unticked.
pub fn enable_autostart_on_first_run(app: &AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    let Ok(dir) = app.path().app_config_dir() else { return };
    let marker = dir.join("autostart-configured");
    if marker.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    if std::fs::write(&marker, b"").is_ok() {
        let _ = app.autolaunch().enable();
    }
}
