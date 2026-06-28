//! Paste2SSH for Linux — Tauri entry point, command surface, and app wiring.

mod clipboard;
mod config;
mod imageutil;
mod login_item;
mod paths;
mod ssh;
mod ssh_config;
mod state;
mod tray;
mod watcher;

use std::path::PathBuf;

use tauri::{Manager, State};
use tauri_plugin_opener::OpenerExt;

use ssh_config::ConnectionDetails;
use state::{AppState, SettingsPatch, Shared, StateSnapshot};

/// The three things that can be uploaded. Clipboard captures arrive as encoded
/// PNG bytes; screenshots and dropped files arrive as paths (screenshots get a
/// generated remote name, dropped files keep their original name).
pub enum ImageInput {
    ClipboardPng { png: Vec<u8> },
    LocalFile(PathBuf),
    RawFile(PathBuf),
}

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> StateSnapshot {
    state.snapshot()
}

#[tauri::command]
fn set_on(on: bool, state: State<'_, AppState>) {
    state.inner().set_on(on);
}

#[tauri::command]
fn toggle(state: State<'_, AppState>) {
    state.inner().toggle();
}

#[tauri::command]
fn select_host(host: String, state: State<'_, AppState>) {
    state.inner().select_host(&host);
}

#[tauri::command]
fn reload_hosts(state: State<'_, AppState>) {
    state.inner().reload_hosts();
}

#[tauri::command]
fn save_settings(patch: SettingsPatch, state: State<'_, AppState>) {
    state.inner().save_settings(patch);
}

#[tauri::command]
fn test_connection(state: State<'_, AppState>) {
    state.inner().test_connection();
}

#[tauri::command]
fn save_ssh_connection(
    original_host: Option<String>,
    alias: String,
    host_name: String,
    user: String,
    port: String,
    remote_dir: String,
    state: State<'_, AppState>,
) -> String {
    state
        .inner()
        .save_ssh_connection(original_host, alias, host_name, user, port, remote_dir)
}

#[tauri::command]
fn delete_ssh_connection(host: String, state: State<'_, AppState>) -> String {
    state.inner().delete_ssh_connection(host)
}

#[tauri::command]
fn connection_details(host: String, state: State<'_, AppState>) -> ConnectionDetails {
    state.inner().connection_details(&host)
}

#[tauri::command]
fn copy_path(path: String, state: State<'_, AppState>) {
    state.inner().copy_path(&path);
}

#[tauri::command]
fn copy_last_path(state: State<'_, AppState>) {
    state.inner().copy_last_path();
}

#[tauri::command]
fn open_ssh_config(app: tauri::AppHandle) {
    let path = ssh_config::ensure_ssh_config();
    let _ = app
        .opener()
        .open_path(path.to_string_lossy().to_string(), None::<&str>);
}

#[tauri::command]
fn set_launch_at_login(on: bool, state: State<'_, AppState>) -> Result<(), String> {
    state.inner().set_launch_at_login(on)
}

#[tauri::command]
fn open_url(url: String, app: tauri::AppHandle) {
    let _ = app.opener().open_url(url, None::<&str>);
}

#[tauri::command]
fn upload_files(paths: Vec<String>, state: State<'_, AppState>) {
    state.inner().upload_files(paths);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single-instance must be registered first so a second launch just
        // re-shows the running window instead of starting over.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tray::show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_state,
            set_on,
            toggle,
            select_host,
            reload_hosts,
            save_settings,
            test_connection,
            save_ssh_connection,
            delete_ssh_connection,
            connection_details,
            copy_path,
            copy_last_path,
            open_ssh_config,
            set_launch_at_login,
            open_url,
            upload_files,
        ])
        .on_menu_event(|app, event| tray::handle_menu_event(app, event.id().as_ref()))
        .setup(|app| {
            let handle = app.handle().clone();
            let shared = Shared::new(handle.clone());
            app.manage(shared.clone());
            // Non-fatal: a minimal WM may lack a StatusNotifierItem host.
            if let Err(e) = tray::build(&handle, &shared) {
                eprintln!("paste2ssh: tray init failed: {e}");
            }
            shared.start_services();
            Ok(())
        })
        // Closing quits (Linux convention); minimizing keeps Paste Mode running
        // in the background. The tray is a bonus quick-toggle where the desktop
        // shows StatusNotifierItems.
        .run(tauri::generate_context!())
        .expect("error while running Paste2SSH");
}
