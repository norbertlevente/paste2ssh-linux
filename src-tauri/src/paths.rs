//! Centralized XDG-style paths. On Linux these resolve under ~/.config and
//! ~/.cache; on macOS (used only for `cargo tauri dev`) they fall back to the
//! platform equivalents, which is fine for development.

use std::path::PathBuf;

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// ~/.config/paste2ssh
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join("paste2ssh")
}

/// ~/.cache/paste2ssh
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| home().join(".cache"))
        .join("paste2ssh")
}

/// ~/.config/paste2ssh/settings.json
pub fn settings_file() -> PathBuf {
    config_dir().join("settings.json")
}

/// ~/.config/paste2ssh/recent.json
pub fn recents_file() -> PathBuf {
    config_dir().join("recent.json")
}

/// ~/.config/autostart/paste2ssh.desktop
pub fn autostart_file() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join("autostart")
        .join("paste2ssh.desktop")
}

/// ~/.ssh/config
pub fn ssh_config_file() -> PathBuf {
    home().join(".ssh").join("config")
}
