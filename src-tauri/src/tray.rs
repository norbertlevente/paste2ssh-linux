//! System-tray icon + menu (StatusNotifierItem on Linux). Mirrors
//! `MenuBarContent.swift`: a status line, on/off toggle, a host submenu with
//! checkmarks, copy-last-path, open/settings, and quit. Icons are drawn in code
//! (a filled disc tinted per state) so there are no extra icon files to ship.

use std::time::Duration;

use tauri::image::Image;
use tauri::menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

const TRAY_ID: &str = "main";

#[derive(Clone, Copy)]
pub enum TrayState {
    Off,
    Connecting,
    On,
    Pulse,
}

impl TrayState {
    fn color(self) -> (u8, u8, u8) {
        match self {
            TrayState::Off => (176, 176, 176),
            TrayState::Connecting => (230, 170, 40),
            TrayState::On => (71, 168, 148),    // teal #47A894
            TrayState::Pulse => (102, 217, 194), // bright teal #66D9C2
        }
    }
}

/// Build the tray icon with its initial (empty-state) menu.
pub fn build(app: &AppHandle, state: &AppState) -> tauri::Result<()> {
    let menu = build_menu(app, state)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(make_icon(TrayState::Off))
        .icon_as_template(false)
        .tooltip("Paste2SSH")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Rebuild the menu and refresh the icon to match current state.
pub fn refresh(app: &AppHandle, state: &AppState) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if let Ok(menu) = build_menu(app, state) {
            let _ = tray.set_menu(Some(menu));
        }
        let _ = tray.set_icon(Some(make_icon(base_state(state))));
    }
}

/// Briefly flash the pulse icon during an upload, then restore the base icon.
pub fn pulse(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_icon(Some(make_icon(TrayState::Pulse)));
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1200));
        if let Some(state) = app.try_state::<AppState>() {
            let state = state.inner().clone();
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                let _ = tray.set_icon(Some(make_icon(base_state(&state))));
            }
        }
    });
}

fn base_state(state: &AppState) -> TrayState {
    if state.is_connecting() {
        TrayState::Connecting
    } else if state.is_on() {
        TrayState::On
    } else {
        TrayState::Off
    }
}

fn build_menu(app: &AppHandle, state: &AppState) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    let is_on = state.is_on();
    let connecting = state.is_connecting();
    let host = state.current_host();
    let hosts = state.hosts();
    let last_path = state.last_path();

    let status_label = if connecting {
        format!("Connecting to {host}…")
    } else if is_on {
        "Paste mode: On".to_string()
    } else {
        "Paste mode: Off".to_string()
    };
    let status = MenuItemBuilder::with_id("status", status_label)
        .enabled(false)
        .build(app)?;

    let toggle_label = if is_on { "Turn Off" } else { "Turn On" };
    let toggle = MenuItemBuilder::with_id("toggle", toggle_label)
        .enabled(!connecting && !host.is_empty())
        .build(app)?;

    // Host submenu
    let mut hosts_sub = SubmenuBuilder::new(app, "Host");
    if hosts.is_empty() {
        let none = MenuItemBuilder::with_id("hosts_none", "No SSH config hosts found")
            .enabled(false)
            .build(app)?;
        hosts_sub = hosts_sub.item(&none);
    } else {
        for h in &hosts {
            let item = CheckMenuItemBuilder::with_id(format!("host:{h}"), h)
                .checked(h == &host)
                .build(app)?;
            hosts_sub = hosts_sub.item(&item);
        }
    }
    let hosts_sub = hosts_sub
        .separator()
        .text("reload_hosts", "Reload SSH Config")
        .text("add_host", "Add SSH Connection…")
        .build()?;

    let copy_last = MenuItemBuilder::with_id("copy_last", "Copy Last Path")
        .enabled(last_path.is_some())
        .build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&status)
        .separator()
        .item(&toggle)
        .item(&hosts_sub)
        .separator()
        .item(&copy_last)
        .separator()
        .text("open", "Open Paste2SSH…")
        .text("settings", "Settings…")
        .separator()
        .text("quit", "Quit")
        .build()?;

    Ok(menu)
}

/// Dispatch a tray (or any) menu click. Wired from the app-level menu handler.
pub fn handle_menu_event(app: &AppHandle, id: &str) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let state = state.inner().clone();

    match id {
        "toggle" => state.toggle(),
        "reload_hosts" => state.reload_hosts(),
        "copy_last" => state.copy_last_path(),
        "open" => show_main_window(app),
        "settings" => {
            show_main_window(app);
            navigate(app, "settings");
        }
        "add_host" => {
            show_main_window(app);
            navigate(app, "add_host");
        }
        "quit" => app.exit(0),
        other if other.starts_with("host:") => state.select_host(&other[5..]),
        _ => {}
    }
}

pub fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

fn navigate(app: &AppHandle, page: &str) {
    let _ = app.emit("navigate", page);
}

/// Draw a filled, anti-aliased disc tinted for the given state.
fn make_icon(state: TrayState) -> Image<'static> {
    let (r, g, b) = state.color();
    let size: u32 = 32;
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let center = (size as f32 - 1.0) / 2.0;
    let radius = 12.5f32;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let alpha = if dist <= radius {
                255
            } else if dist <= radius + 1.0 {
                (255.0 * (radius + 1.0 - dist)).clamp(0.0, 255.0) as u8
            } else {
                0
            };
            let i = ((y * size + x) * 4) as usize;
            buf[i] = r;
            buf[i + 1] = g;
            buf[i + 2] = b;
            buf[i + 3] = alpha;
        }
    }

    Image::new_owned(buf, size, size)
}
