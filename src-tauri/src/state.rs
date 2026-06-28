//! Central application state (port of `AppState.swift`). Holds settings, the
//! on/off + connection phase, recent uploads, and per-host readiness; owns the
//! upload pipeline and the clipboard/screenshot watchers; and pushes a
//! `StateSnapshot` to the webview after every change. Methods that touch the
//! tray or run ssh take `self: &Arc<Self>` so they can spawn workers and refresh
//! the tray.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Local;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::clipboard::{self, ClipboardHandle};
use crate::config::{self, Settings};
use crate::imageutil::Dedup;
use crate::paths;
use crate::ssh::SshUploader;
use crate::ssh_config;
use crate::tray;
use crate::watcher::{self, ScreenshotWatcher};
use crate::{login_item, ImageInput};

pub type AppState = Arc<Shared>;

const RECENT_CAP: usize = 100;

#[derive(Clone, Copy, PartialEq)]
pub enum StatusKind {
    Ready,
    Working,
    Success,
    Error,
}

impl StatusKind {
    fn as_str(self) -> &'static str {
        match self {
            StatusKind::Ready => "ready",
            StatusKind::Working => "working",
            StatusKind::Success => "success",
            StatusKind::Error => "error",
        }
    }
}

#[derive(Clone)]
enum Readiness {
    Checking,
    Ready,
    Failed(String),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct LastUpload {
    pub local_name: String,
    pub remote_path: String,
    pub host: String,
    pub date_ms: i64,
}

struct Runtime {
    settings: Settings,
    is_on: bool,
    is_connecting: bool,
    status_text: String,
    status_kind: StatusKind,
    last_upload: Option<LastUpload>,
    recent_uploads: Vec<LastUpload>,
    hosts: Vec<String>,
    readiness: HashMap<String, Readiness>,
    first_success_hosts: HashSet<String>,
    active_uploads: HashSet<String>,
    test_result: String,
}

pub struct Shared {
    rt: Mutex<Runtime>,
    uploader: SshUploader,
    dedup: Dedup,
    clipboard: Mutex<Option<ClipboardHandle>>,
    screenshot: Mutex<Option<ScreenshotWatcher>>,
    app: AppHandle,
}

impl Shared {
    pub fn new(app: AppHandle) -> Arc<Shared> {
        let mut settings = Settings::load();
        // Reconcile launch-at-login with reality so the UI never lies.
        settings.launch_at_login = login_item::is_enabled();

        let runtime = Runtime {
            settings,
            is_on: false,
            is_connecting: false,
            status_text: "Ready.".to_string(),
            status_kind: StatusKind::Ready,
            last_upload: None,
            recent_uploads: load_recents(),
            hosts: ssh_config::load_hosts(),
            readiness: HashMap::new(),
            first_success_hosts: load_first_success(),
            active_uploads: HashSet::new(),
            test_result: String::new(),
        };
        let last = runtime.recent_uploads.first().cloned();

        let shared = Arc::new(Shared {
            rt: Mutex::new(Runtime {
                last_upload: last,
                ..runtime
            }),
            uploader: SshUploader::new(),
            dedup: Dedup::new(32),
            clipboard: Mutex::new(None),
            screenshot: Mutex::new(None),
            app,
        });
        shared.rt.lock().unwrap().settings.save();
        shared
    }

    /// Start the always-on clipboard service and restore the persisted on-state.
    pub fn start_services(self: &Arc<Self>) {
        let me = self.clone();
        let handle = clipboard::start(move |png, hash| {
            me.on_clipboard_image(png, hash);
        });
        *self.clipboard.lock().unwrap() = Some(handle);

        let restore_on = load_restore_on();
        let host_ok = !self.rt.lock().unwrap().settings.normalized_host().is_empty();
        if restore_on && host_ok {
            self.set_on(true);
        } else {
            self.ui_refresh();
        }
    }

    // --- simple getters (for the tray) ------------------------------------------

    pub fn is_on(&self) -> bool {
        self.rt.lock().unwrap().is_on
    }
    pub fn is_connecting(&self) -> bool {
        self.rt.lock().unwrap().is_connecting
    }
    pub fn current_host(&self) -> String {
        self.rt.lock().unwrap().settings.normalized_host()
    }
    pub fn hosts(&self) -> Vec<String> {
        self.rt.lock().unwrap().hosts.clone()
    }
    pub fn last_path(&self) -> Option<String> {
        self.rt
            .lock()
            .unwrap()
            .last_upload
            .as_ref()
            .map(|u| u.remote_path.clone())
    }

    fn clipboard(&self) -> Option<ClipboardHandle> {
        self.clipboard.lock().unwrap().clone()
    }

    // --- snapshot + refresh ------------------------------------------------------

    pub fn snapshot(&self) -> StateSnapshot {
        let rt = self.rt.lock().unwrap();
        let host = rt.settings.normalized_host();
        let phase = if rt.is_connecting {
            "connecting"
        } else if rt.is_on {
            "on"
        } else {
            "off"
        };
        let readiness = rt
            .readiness
            .iter()
            .map(|(k, v)| (k.clone(), ReadinessDto::from(v)))
            .collect();
        let recent = rt
            .recent_uploads
            .iter()
            .map(RecentDto::from)
            .collect::<Vec<_>>();

        StateSnapshot {
            phase: phase.to_string(),
            is_on: rt.is_on,
            is_connecting: rt.is_connecting,
            status_text: rt.status_text.clone(),
            status_kind: rt.status_kind.as_str().to_string(),
            host: host.clone(),
            remote_dir: rt.settings.remote_dir_for(&host),
            hosts: rt.hosts.clone(),
            readiness,
            recent,
            last_path: rt.last_upload.as_ref().map(|u| u.remote_path.clone()),
            test_result: rt.test_result.clone(),
            settings: SettingsDto::from(&rt.settings),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    fn emit(&self) {
        let _ = self.app.emit("state", self.snapshot());
    }

    fn ui_refresh(self: &Arc<Self>) {
        self.emit();
        tray::refresh(&self.app, self);
    }

    fn set_status(self: &Arc<Self>, text: impl Into<String>, kind: StatusKind) {
        {
            let mut rt = self.rt.lock().unwrap();
            rt.status_text = text.into();
            rt.status_kind = kind;
        }
        self.ui_refresh();
    }

    fn spawn<F>(self: &Arc<Self>, f: F)
    where
        F: FnOnce(Arc<Shared>) + Send + 'static,
    {
        let me = self.clone();
        std::thread::spawn(move || f(me));
    }

    // --- on/off + host selection -------------------------------------------------

    pub fn toggle(self: &Arc<Self>) {
        if self.is_on() || self.is_connecting() {
            self.set_on(false);
        } else {
            self.set_on(true);
        }
    }

    pub fn set_on(self: &Arc<Self>, on: bool) {
        if !on {
            {
                let mut rt = self.rt.lock().unwrap();
                rt.is_connecting = false;
                rt.is_on = false;
                rt.last_upload = None;
                rt.status_text = "Paste mode off.".to_string();
                rt.status_kind = StatusKind::Ready;
            }
            self.stop_watchers();
            save_restore_on(false);
            let settings = self.rt.lock().unwrap().settings.clone();
            self.spawn(move |me| me.uploader.close_master(&settings));
            self.ui_refresh();
            return;
        }

        if self.is_on() || self.is_connecting() {
            return;
        }
        {
            let mut rt = self.rt.lock().unwrap();
            if rt.settings.normalized_host().is_empty() {
                drop(rt);
                self.set_status("Pick an SSH host first.", StatusKind::Error);
                return;
            }
            rt.settings.monitor_clipboard = true;
            rt.settings.auto_copy_path = true;
            rt.is_connecting = true;
            rt.status_text = format!("Connecting to {}…", rt.settings.normalized_host());
            rt.status_kind = StatusKind::Working;
            rt.settings.save();
        }
        self.ui_refresh();

        let settings = self.rt.lock().unwrap().settings.clone();
        self.spawn(move |me| {
            let result = me.uploader.test_connection(&settings);
            let ok = result == "Connection OK.";
            {
                let mut rt = me.rt.lock().unwrap();
                rt.is_connecting = false;
                rt.test_result = result.clone();
                rt.readiness.insert(
                    settings.normalized_host(),
                    if ok {
                        Readiness::Ready
                    } else {
                        Readiness::Failed(result.clone())
                    },
                );
                if ok {
                    rt.is_on = true;
                    rt.status_text =
                        "Connected. Copy a screenshot, then paste the remote path.".to_string();
                    rt.status_kind = StatusKind::Success;
                } else {
                    rt.is_on = false;
                    rt.status_text = result;
                    rt.status_kind = StatusKind::Error;
                }
            }
            if ok {
                me.start_watchers();
                save_restore_on(true);
            }
            me.ui_refresh();
        });
    }

    pub fn select_host(self: &Arc<Self>, host: &str) {
        let host = host.to_string();
        let (previous_host, was_on, old_settings) = {
            let rt = self.rt.lock().unwrap();
            (
                rt.settings.normalized_host(),
                rt.is_on,
                rt.settings.clone(),
            )
        };

        if was_on {
            self.stop_watchers();
        }
        {
            let mut rt = self.rt.lock().unwrap();
            rt.is_connecting = false;
        }

        if !previous_host.is_empty() && previous_host != host.trim() {
            self.spawn(move |me| me.uploader.close_master(&old_settings));
        }

        {
            let mut rt = self.rt.lock().unwrap();
            rt.settings.host = host.clone();
            rt.settings.save();
            rt.last_upload = None;
            rt.test_result.clear();
        }

        if host.is_empty() {
            {
                let mut rt = self.rt.lock().unwrap();
                if was_on {
                    rt.is_on = false;
                }
                rt.status_text = "Pick an SSH host first.".to_string();
                rt.status_kind = StatusKind::Ready;
            }
            self.ui_refresh();
            return;
        }

        if was_on {
            {
                let mut rt = self.rt.lock().unwrap();
                rt.is_connecting = true;
                rt.status_text = format!("Switching to {host}…");
                rt.status_kind = StatusKind::Working;
            }
            self.ui_refresh();
            let settings = self.rt.lock().unwrap().settings.clone();
            self.spawn(move |me| {
                let result = me.uploader.test_connection(&settings);
                let ok = result == "Connection OK.";
                {
                    let mut rt = me.rt.lock().unwrap();
                    rt.is_connecting = false;
                    rt.test_result = result.clone();
                    rt.readiness.insert(
                        settings.normalized_host(),
                        if ok {
                            Readiness::Ready
                        } else {
                            Readiness::Failed(result.clone())
                        },
                    );
                    if ok {
                        rt.status_text = format!("Connected to {}.", settings.normalized_host());
                        rt.status_kind = StatusKind::Success;
                    } else {
                        rt.is_on = false;
                        rt.status_text = result;
                        rt.status_kind = StatusKind::Error;
                    }
                }
                if ok {
                    me.start_watchers();
                }
                me.ui_refresh();
            });
        } else {
            self.set_status(format!("Ready for {host}."), StatusKind::Ready);
            self.silently_check_host(&host);
        }
    }

    fn silently_check_host(self: &Arc<Self>, host: &str) {
        let host = host.trim().to_string();
        if host.is_empty() {
            return;
        }
        {
            let mut rt = self.rt.lock().unwrap();
            rt.readiness.insert(host.clone(), Readiness::Checking);
        }
        self.ui_refresh();
        let mut settings = self.rt.lock().unwrap().settings.clone();
        settings.host = host.clone();
        self.spawn(move |me| {
            let result = me.uploader.test_connection(&settings);
            let ok = result == "Connection OK.";
            {
                let mut rt = me.rt.lock().unwrap();
                rt.readiness.insert(
                    host,
                    if ok {
                        Readiness::Ready
                    } else {
                        Readiness::Failed(result)
                    },
                );
            }
            me.ui_refresh();
        });
    }

    pub fn reload_hosts(self: &Arc<Self>) {
        {
            let mut rt = self.rt.lock().unwrap();
            rt.hosts = ssh_config::load_hosts();
        }
        self.ui_refresh();
    }

    // --- watchers ----------------------------------------------------------------

    fn start_watchers(self: &Arc<Self>) {
        let (monitor_clipboard, monitor_screenshots, folder) = {
            let rt = self.rt.lock().unwrap();
            (
                rt.settings.monitor_clipboard,
                rt.settings.monitor_screenshots,
                rt.settings.screenshot_folder_path(),
            )
        };

        if let Some(cb) = self.clipboard() {
            cb.set_watching(monitor_clipboard);
        }

        let mut slot = self.screenshot.lock().unwrap();
        *slot = None;
        if monitor_screenshots {
            let me = self.clone();
            *slot = watcher::start(folder, move |path| {
                me.on_screenshot_file(path);
            });
        }
    }

    fn stop_watchers(self: &Arc<Self>) {
        if let Some(cb) = self.clipboard() {
            cb.set_watching(false);
        }
        *self.screenshot.lock().unwrap() = None;
    }

    fn restart_watchers(self: &Arc<Self>) {
        if self.is_on() {
            self.stop_watchers();
            self.start_watchers();
        }
    }

    // --- image sources -----------------------------------------------------------

    fn on_clipboard_image(self: &Arc<Self>, png: Vec<u8>, pixel_hash: String) {
        if self.dedup.seen_or_insert(&pixel_hash) {
            return;
        }
        self.spawn(move |me| {
            me.run_pipeline(ImageInput::ClipboardPng { png }, false);
        });
    }

    fn on_screenshot_file(self: &Arc<Self>, path: PathBuf) {
        if let Some(hash) = crate::imageutil::pixel_hash_file(&path) {
            if self.dedup.seen_or_insert(&hash) {
                return;
            }
        }
        self.spawn(move |me| {
            me.run_pipeline(ImageInput::LocalFile(path), false);
        });
    }

    pub fn upload_files(self: &Arc<Self>, paths: Vec<String>) {
        if self.current_host().is_empty() {
            self.set_status("Pick an SSH host first.", StatusKind::Error);
            return;
        }
        if self.is_connecting() {
            self.set_status("Connecting… try again in a moment.", StatusKind::Error);
            return;
        }

        let mut files: Vec<PathBuf> = Vec::new();
        let mut skipped = 0usize;
        for raw in paths {
            let path = PathBuf::from(raw);
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => files.push(path),
                _ => skipped += 1,
            }
        }

        if files.is_empty() {
            let msg = if skipped > 0 {
                "Folders can't be uploaded. Drop files instead."
            } else {
                "Nothing to upload."
            };
            self.set_status(msg, StatusKind::Error);
            return;
        }

        self.spawn(move |me| {
            let auto_copy = me.rt.lock().unwrap().settings.auto_copy_path;
            let mut uploaded: Vec<String> = Vec::new();
            for path in files {
                let before = me.last_path();
                me.run_pipeline(ImageInput::RawFile(path), true);
                let after = me.last_path();
                if let Some(p) = after {
                    if Some(&p) != before.as_ref() {
                        uploaded.push(p);
                    }
                }
            }
            if uploaded.is_empty() {
                return;
            }
            if auto_copy {
                if let Some(cb) = me.clipboard() {
                    cb.set_text(uploaded.join("\n"));
                }
            }
            let mut text = match uploaded.len() {
                1 => if auto_copy { "Uploaded and copied remote path." } else { "Uploaded." }.to_string(),
                n => {
                    if auto_copy {
                        format!("Uploaded {n} files and copied their paths.")
                    } else {
                        format!("Uploaded {n} files.")
                    }
                }
            };
            if skipped > 0 {
                text.push_str(&format!(" Skipped {skipped} folder{}.", if skipped == 1 { "" } else { "s" }));
            }
            me.set_status(text, StatusKind::Success);
        });
    }

    /// The upload pipeline (port of `AppState.uploadPipeline`).
    fn run_pipeline(self: &Arc<Self>, input: ImageInput, force: bool) {
        let (is_on, settings) = {
            let rt = self.rt.lock().unwrap();
            (rt.is_on, rt.settings.clone())
        };
        if !is_on && !force {
            return;
        }
        if settings.normalized_host().is_empty() {
            self.set_status("Pick an SSH host first.", StatusKind::Error);
            return;
        }

        let now = Local::now();
        let filename = match &input {
            ImageInput::RawFile(path) => config::sanitized_remote_name(path),
            _ => settings.generated_filename(now),
        };

        let prepared = match prepare(input, &filename) {
            Ok(prepared) => prepared,
            Err(err) => {
                self.set_status(err, StatusKind::Error);
                return;
            }
        };

        let key = prepared.local_path.to_string_lossy().into_owned();
        {
            let mut rt = self.rt.lock().unwrap();
            if rt.active_uploads.contains(&key) {
                return;
            }
            rt.active_uploads.insert(key.clone());
        }
        self.set_status(format!("Uploading {}…", prepared.display_name), StatusKind::Working);

        let result = self
            .uploader
            .upload(&prepared.local_path, &filename, &settings);

        self.rt.lock().unwrap().active_uploads.remove(&key);

        match result {
            Ok(upload) => {
                let auto_copy = settings.auto_copy_path;
                if auto_copy {
                    if let Some(cb) = self.clipboard() {
                        cb.set_text(upload.remote_path.clone());
                    }
                }
                let last = LastUpload {
                    local_name: prepared.display_name,
                    remote_path: upload.remote_path.clone(),
                    host: settings.normalized_host(),
                    date_ms: now.timestamp_millis(),
                };
                let first_success;
                {
                    let mut rt = self.rt.lock().unwrap();
                    rt.status_text = if auto_copy {
                        "Uploaded and copied remote path.".to_string()
                    } else {
                        "Uploaded.".to_string()
                    };
                    rt.status_kind = StatusKind::Success;
                    rt.last_upload = Some(last.clone());
                    rt.recent_uploads.insert(0, last.clone());
                    rt.recent_uploads.truncate(RECENT_CAP);
                    first_success = !rt.first_success_hosts.contains(&last.host);
                    if first_success {
                        rt.first_success_hosts.insert(last.host.clone());
                    }
                }
                save_recents(&self.rt.lock().unwrap().recent_uploads);
                if first_success {
                    save_first_success(&self.rt.lock().unwrap().first_success_hosts);
                    let _ = self.app.emit("first-success", &last.host);
                }
                self.ui_refresh();
                tray::pulse(&self.app);
            }
            Err(message) => {
                self.set_status(message, StatusKind::Error);
            }
        }
    }

    // --- commands backing ---------------------------------------------------------

    pub fn copy_path(self: &Arc<Self>, path: &str) {
        if let Some(cb) = self.clipboard() {
            cb.set_text(path.to_string());
        }
        self.set_status("Copied remote path.", StatusKind::Success);
    }

    pub fn copy_last_path(self: &Arc<Self>) {
        match self.last_path() {
            Some(path) => self.copy_path(&path),
            None => self.set_status("No upload yet.", StatusKind::Error),
        }
    }

    pub fn connection_details(&self, host: &str) -> ssh_config::ConnectionDetails {
        let settings = self.rt.lock().unwrap().settings.clone();
        ssh_config::connection_details(&settings, host)
    }

    pub fn save_settings(self: &Arc<Self>, patch: SettingsPatch) {
        {
            let mut rt = self.rt.lock().unwrap();
            patch.apply_to(&mut rt.settings);
            rt.settings.save();
        }
        self.restart_watchers();
        self.ui_refresh();
    }

    pub fn test_connection(self: &Arc<Self>) {
        {
            let mut rt = self.rt.lock().unwrap();
            rt.test_result = "Testing…".to_string();
            rt.status_text = "Testing SSH connection…".to_string();
            rt.status_kind = StatusKind::Working;
        }
        self.ui_refresh();
        let settings = self.rt.lock().unwrap().settings.clone();
        self.spawn(move |me| {
            let result = me.uploader.test_connection(&settings);
            let ok = result == "Connection OK.";
            {
                let mut rt = me.rt.lock().unwrap();
                rt.test_result = result.clone();
                rt.status_text = result.clone();
                rt.status_kind = if ok { StatusKind::Success } else { StatusKind::Error };
                rt.readiness.insert(
                    settings.normalized_host(),
                    if ok { Readiness::Ready } else { Readiness::Failed(result) },
                );
            }
            me.ui_refresh();
        });
    }

    pub fn save_ssh_connection(
        self: &Arc<Self>,
        original_host: Option<String>,
        alias: String,
        host_name: String,
        user: String,
        port: String,
        remote_dir: String,
    ) -> String {
        match ssh_config::save_connection(original_host.as_deref(), &alias, &host_name, &user, &port) {
            Ok(saved_alias) => {
                {
                    let mut rt = self.rt.lock().unwrap();
                    let default_dir = rt.settings.remote_dir.clone();
                    let dir = if remote_dir.trim().is_empty() {
                        default_dir.clone()
                    } else {
                        remote_dir.clone()
                    };
                    set_remote_dir(&mut rt.settings, &saved_alias, &dir);
                    if let Some(original) = &original_host {
                        if original != &saved_alias {
                            rt.settings.remote_dirs_by_host.remove(original);
                        }
                    }
                    rt.settings.save();
                }
                self.reload_hosts();
                self.select_host(&saved_alias);
                if original_host.is_some() {
                    format!("Saved {saved_alias}.")
                } else {
                    format!("Added {saved_alias}.")
                }
            }
            Err(message) => message,
        }
    }

    pub fn delete_ssh_connection(self: &Arc<Self>, host: String) -> String {
        match ssh_config::delete_connection(&host) {
            Ok(()) => {
                {
                    let mut rt = self.rt.lock().unwrap();
                    rt.settings.remote_dirs_by_host.remove(&host);
                    if rt.settings.normalized_host() == host {
                        rt.settings.host.clear();
                        rt.is_on = false;
                        rt.last_upload = None;
                    }
                    rt.settings.save();
                }
                self.stop_watchers();
                self.reload_hosts();
                format!("Deleted {host}.")
            }
            Err(message) => message,
        }
    }

    pub fn set_launch_at_login(self: &Arc<Self>, on: bool) -> Result<(), String> {
        {
            let mut rt = self.rt.lock().unwrap();
            rt.settings.launch_at_login = on;
            rt.settings.save();
        }
        let result = login_item::set_enabled(on);
        if result.is_err() {
            // Reconcile the toggle to reality so the UI doesn't lie.
            let actual = login_item::is_enabled();
            let mut rt = self.rt.lock().unwrap();
            rt.settings.launch_at_login = actual;
            rt.settings.save();
        }
        self.ui_refresh();
        result
    }
}

// --- prepared input -----------------------------------------------------------

struct Prepared {
    local_path: PathBuf,
    display_name: String,
}

fn prepare(input: ImageInput, filename: &str) -> Result<Prepared, String> {
    match input {
        ImageInput::ClipboardPng { png } => {
            let dir = std::env::temp_dir().join("paste2ssh");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let path = dir.join(filename);
            std::fs::write(&path, png).map_err(|e| e.to_string())?;
            Ok(Prepared {
                local_path: path,
                display_name: filename.to_string(),
            })
        }
        ImageInput::LocalFile(path) | ImageInput::RawFile(path) => {
            let display = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| filename.to_string());
            Ok(Prepared {
                local_path: path,
                display_name: display,
            })
        }
    }
}

fn set_remote_dir(settings: &mut Settings, host: &str, remote_dir: &str) {
    let key = host.trim();
    if key.is_empty() {
        settings.remote_dir = remote_dir.to_string();
        return;
    }
    let trimmed = remote_dir.trim();
    if trimmed.is_empty() || trimmed == settings.remote_dir.trim() {
        settings.remote_dirs_by_host.remove(key);
    } else {
        settings.remote_dirs_by_host.insert(key.to_string(), trimmed.to_string());
    }
}

// --- DTOs ---------------------------------------------------------------------

#[derive(Serialize, Clone)]
pub struct StateSnapshot {
    pub phase: String,
    pub is_on: bool,
    pub is_connecting: bool,
    pub status_text: String,
    pub status_kind: String,
    pub host: String,
    pub remote_dir: String,
    pub hosts: Vec<String>,
    pub readiness: HashMap<String, ReadinessDto>,
    pub recent: Vec<RecentDto>,
    pub last_path: Option<String>,
    pub test_result: String,
    pub settings: SettingsDto,
    pub version: String,
}

#[derive(Serialize, Clone)]
pub struct ReadinessDto {
    pub state: String,
    pub message: String,
}

impl From<&Readiness> for ReadinessDto {
    fn from(value: &Readiness) -> Self {
        match value {
            Readiness::Checking => ReadinessDto { state: "checking".into(), message: String::new() },
            Readiness::Ready => ReadinessDto { state: "ready".into(), message: String::new() },
            Readiness::Failed(m) => ReadinessDto { state: "failed".into(), message: m.clone() },
        }
    }
}

#[derive(Serialize, Clone)]
pub struct RecentDto {
    pub local_name: String,
    pub remote_path: String,
    pub host: String,
    pub date_ms: i64,
}

impl From<&LastUpload> for RecentDto {
    fn from(u: &LastUpload) -> Self {
        RecentDto {
            local_name: u.local_name.clone(),
            remote_path: u.remote_path.clone(),
            host: u.host.clone(),
            date_ms: u.date_ms,
        }
    }
}

#[derive(Serialize, Clone)]
pub struct SettingsDto {
    pub remote_dir: String,
    pub filename_pattern: String,
    pub screenshot_folder: String,
    pub auto_copy_path: bool,
    pub monitor_screenshots: bool,
    pub launch_at_login: bool,
}

impl From<&Settings> for SettingsDto {
    fn from(s: &Settings) -> Self {
        SettingsDto {
            remote_dir: s.remote_dir.clone(),
            filename_pattern: s.filename_pattern.clone(),
            screenshot_folder: s.screenshot_folder.clone(),
            auto_copy_path: s.auto_copy_path,
            monitor_screenshots: s.monitor_screenshots,
            launch_at_login: s.launch_at_login,
        }
    }
}

#[derive(Deserialize)]
pub struct SettingsPatch {
    pub remote_dir: Option<String>,
    pub filename_pattern: Option<String>,
    pub screenshot_folder: Option<String>,
    pub auto_copy_path: Option<bool>,
    pub monitor_screenshots: Option<bool>,
}

impl SettingsPatch {
    fn apply_to(&self, settings: &mut Settings) {
        if let Some(v) = &self.remote_dir {
            settings.remote_dir = v.clone();
        }
        if let Some(v) = &self.filename_pattern {
            settings.filename_pattern = v.clone();
        }
        if let Some(v) = &self.screenshot_folder {
            settings.screenshot_folder = v.clone();
        }
        if let Some(v) = self.auto_copy_path {
            settings.auto_copy_path = v;
        }
        if let Some(v) = self.monitor_screenshots {
            settings.monitor_screenshots = v;
        }
    }
}

// --- persistence helpers ------------------------------------------------------

fn load_recents() -> Vec<LastUpload> {
    std::fs::read(paths::recents_file())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_recents(recents: &[LastUpload]) {
    let path = paths::recents_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec(recents) {
        let _ = std::fs::write(path, json);
    }
}

fn load_first_success() -> HashSet<String> {
    std::fs::read(paths::config_dir().join("first_success.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok())
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

fn save_first_success(set: &HashSet<String>) {
    let path = paths::config_dir().join("first_success.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let list: Vec<&String> = set.iter().collect();
    if let Ok(json) = serde_json::to_vec(&list) {
        let _ = std::fs::write(path, json);
    }
}

fn load_restore_on() -> bool {
    std::fs::read(paths::config_dir().join("state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|v| v.get("is_on").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

fn save_restore_on(on: bool) {
    let path = paths::config_dir().join("state.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{{\"is_on\":{on}}}"));
}
