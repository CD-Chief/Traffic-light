// No console window behind the widget on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod focus;
mod tray;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_window_state::StateFlags;
use traffic_light_core::{serve, Limits, Session, Shared, DEFAULT_PORT};

/// Set from the tray's "Show widget" toggle. The window is visible only when
/// the user wants it *and* there is at least one session.
pub struct Visibility {
    pub wanted: AtomicBool,
}

#[tauri::command]
fn get_sessions(shared: tauri::State<'_, Arc<Shared>>) -> Vec<Session> {
    shared.sessions()
}

#[tauri::command]
fn get_limits(shared: tauri::State<'_, Arc<Shared>>) -> Limits {
    shared.limits()
}

/// Bring the window behind a light to the front. False means nothing found.
#[tauri::command]
fn focus_session(
    window: tauri::WebviewWindow,
    shared: tauri::State<'_, Arc<Shared>>,
    session_id: String,
) -> bool {
    let label = shared
        .sessions()
        .into_iter()
        .find(|s| s.session_id == session_id)
        .map(|s| s.label)
        .unwrap_or_default();
    focus::focus(&session_id, &label, Some(&window))
}

/// Push the current session list to the webview, tray and window visibility.
pub fn publish(app: &AppHandle, sessions: &[Session]) {
    focus::retain(sessions);
    let _ = app.emit("sessions-updated", sessions);
    tray::update(app, sessions);
    if let Some(window) = app.get_webview_window("main") {
        let wanted = app.state::<Visibility>().wanted.load(Ordering::Relaxed);
        let _ = if wanted && !sessions.is_empty() {
            window.show()
        } else {
            window.hide()
        };
    }
}

fn main() {
    tauri::Builder::default()
        // Must be registered first: a second launch exits here instead of
        // fighting over the port.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            app.state::<Visibility>()
                .wanted
                .store(true, Ordering::Relaxed);
            publish(app, &app.state::<Arc<Shared>>().sessions());
        }))
        // Position only: size follows the session count and visibility is ours.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::POSITION)
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(Visibility {
            wanted: AtomicBool::new(true),
        })
        .invoke_handler(tauri::generate_handler![
            get_sessions,
            get_limits,
            focus_session
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            let for_events = handle.clone();
            let shared = Shared::new(
                std::env::var("TRAFFIC_LIGHT_TOKEN").ok(),
                move |sessions, _transition| publish(&for_events, sessions),
            );
            let for_limits = handle.clone();
            shared.set_limits_callback(move |limits| {
                let _ = for_limits.emit("limits-updated", limits);
                let shared = for_limits.state::<Arc<Shared>>();
                tray::update(&for_limits, &shared.sessions());
            });
            shared.set_peer_callback(focus::remember);
            app.manage(shared.clone());

            #[cfg(debug_assertions)]
            focus::debug_test(handle.clone());

            tray::build(&handle)?;
            tray::enable_autostart_on_first_run(&handle);

            let for_error = handle.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = serve(DEFAULT_PORT, shared).await {
                    eprintln!("traffic-light: cannot listen on 127.0.0.1:{DEFAULT_PORT}: {e}");
                    tray::show_error(&for_error, &format!("Port {DEFAULT_PORT} is in use"));
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Traffic Light");
}
