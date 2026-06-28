//! Port of `Settings.swift`. Persisted as JSON at ~/.config/paste2ssh/settings.json.
//!
//! Linux-specific changes from the macOS app:
//! - `remote_dir` defaults to `/tmp/paste2ssh` (the OS clears /tmp on reboot, so
//!   there is no remote cleanup feature to port).
//! - `screenshot_folder` defaults to the XDG screenshots dir (~/Pictures/Screenshots).
//! - `filename_pattern` uses strftime tokens (chrono), e.g. `%Y%m%d-%H%M%S`.
//! - The cleanup/cron and notification fields are dropped entirely.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::paths;

const DEFAULT_REMOTE_DIR: &str = "/tmp/paste2ssh";
const DEFAULT_FILENAME_PATTERN: &str = "screenshot-%Y%m%d-%H%M%S.png";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub host: String,
    pub username: String,
    pub port: Option<u16>,
    pub remote_dir: String,
    pub screenshot_folder: String,
    pub filename_pattern: String,
    pub monitor_clipboard: bool,
    pub monitor_screenshots: bool,
    pub auto_copy_path: bool,
    pub remote_dirs_by_host: HashMap<String, String>,
    pub launch_at_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            host: String::new(),
            username: String::new(),
            port: None,
            remote_dir: DEFAULT_REMOTE_DIR.to_string(),
            screenshot_folder: default_screenshot_folder(),
            filename_pattern: DEFAULT_FILENAME_PATTERN.to_string(),
            monitor_clipboard: true,
            monitor_screenshots: true,
            auto_copy_path: true,
            remote_dirs_by_host: HashMap::new(),
            launch_at_login: false,
        }
    }
}

impl Settings {
    /// Load from disk, falling back to defaults on any error (mirrors the macOS
    /// "never throw, just use defaults" behavior).
    pub fn load() -> Settings {
        let path = paths::settings_file();
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Settings>(&bytes).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    /// Persist to disk; best-effort (directory is created as needed).
    pub fn save(&self) {
        let path = paths::settings_file();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(&path, json);
        }
    }

    pub fn normalized_host(&self) -> String {
        self.host.trim().to_string()
    }

    pub fn normalized_username(&self) -> Option<String> {
        let value = self.username.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    }

    /// `user@host` or just `host`.
    pub fn host_target(&self) -> String {
        match self.normalized_username() {
            Some(user) => format!("{}@{}", user, self.normalized_host()),
            None => self.normalized_host(),
        }
    }

    /// `user@host:port` — used as a stable key for caches and control sockets.
    pub fn display_target(&self) -> String {
        match self.port {
            Some(port) => format!("{}:{}", self.host_target(), port),
            None => self.host_target(),
        }
    }

    /// Remote directory for the currently selected host (per-host override or default).
    pub fn effective_remote_dir(&self) -> String {
        self.remote_dir_for(&self.normalized_host())
    }

    pub fn remote_dir_for(&self, host: &str) -> String {
        let key = host.trim();
        let host_dir = self
            .remote_dirs_by_host
            .get(key)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if !host_dir.is_empty() {
            return host_dir;
        }
        let default_dir = self.remote_dir.trim();
        if default_dir.is_empty() {
            DEFAULT_REMOTE_DIR.to_string()
        } else {
            default_dir.to_string()
        }
    }

    /// Screenshot folder with a leading `~` expanded.
    pub fn screenshot_folder_path(&self) -> PathBuf {
        expand_tilde(&self.screenshot_folder)
    }

    /// Build a filename from the strftime pattern, then sanitize it.
    pub fn generated_filename(&self, now: DateTime<Local>) -> String {
        let pattern = {
            let trimmed = self.filename_pattern.trim();
            if trimmed.is_empty() {
                DEFAULT_FILENAME_PATTERN
            } else {
                trimmed
            }
        };
        let rendered = format_pattern(pattern, now);
        sanitize_filename(&rendered, &now)
    }
}

fn default_screenshot_folder() -> String {
    if let Some(pics) = dirs::picture_dir() {
        let screenshots = pics.join("Screenshots");
        if screenshots.is_dir() {
            return screenshots.to_string_lossy().into_owned();
        }
        return pics.to_string_lossy().into_owned();
    }
    dirs::home_dir()
        .map(|h| h.join("Pictures").to_string_lossy().into_owned())
        .unwrap_or_else(|| "~/Pictures".to_string())
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// Render a strftime pattern without panicking on an invalid specifier: we
/// pre-collect the format items and fall back to the default pattern if any
/// item is an error.
fn format_pattern(pattern: &str, now: DateTime<Local>) -> String {
    use chrono::format::{Item, StrftimeItems};
    let items: Vec<Item> = StrftimeItems::new(pattern).collect();
    if items.iter().any(|it| matches!(it, Item::Error)) {
        return now.format(DEFAULT_FILENAME_PATTERN).to_string();
    }
    now.format_with_items(items.iter()).to_string()
}

/// Port of the sanitize logic in `Settings.generatedFilename`: keep
/// letters/digits/`.`/`_`/`-`, map the rest to `-`, collapse runs of `-`, trim
/// leading/trailing `-`/`.`, and force a `.png` extension when none is present.
fn sanitize_filename(input: &str, now: &DateTime<Local>) -> String {
    let sanitized: String = input
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();

    let mut filename = collapse_dashes(&sanitized);
    filename = filename.trim_matches(|c| c == '-' || c == '.').to_string();

    if filename.is_empty() {
        filename = format!("screenshot-{}.png", now.timestamp());
    }
    if PathBuf::from(&filename).extension().is_none() {
        filename.push_str(".png");
    }
    filename
}

fn collapse_dashes(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut prev_dash = false;
    for c in value.chars() {
        if c == '-' {
            if !prev_dash {
                out.push(c);
            }
            prev_dash = true;
        } else {
            out.push(c);
            prev_dash = false;
        }
    }
    out
}

/// Port of `ImageSource.sanitizedRemoteName`: keep the original stem and
/// extension for an arbitrary dropped file (does NOT force `.png`).
pub fn sanitized_remote_name(path: &std::path::Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let sanitized: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();

    let mut name = collapse_dashes(&sanitized);
    name = name.trim_matches(|c| c == '-' || c == '.').to_string();
    if name.is_empty() {
        name = format!("file-{}", Local::now().timestamp());
    }

    let ext: String = path
        .extension()
        .map(|e| e.to_string_lossy().chars().filter(|c| c.is_alphanumeric()).collect())
        .unwrap_or_default();

    if ext.is_empty() {
        name
    } else {
        format!("{}.{}", name, ext)
    }
}
