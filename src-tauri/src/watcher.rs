//! Screenshot-folder watcher (port of `ScreenshotWatcher.swift`). Uses inotify
//! via the `notify` crate. Pre-existing files are seeded so only NEW files fire;
//! each new file is waited on until its size is stable (so we never scp a
//! half-written capture) before the callback runs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

const ALLOWED_EXT: &[&str] = &["png", "jpg", "jpeg", "webp"];

pub struct ScreenshotWatcher {
    _watcher: notify::RecommendedWatcher,
    stop: Arc<AtomicBool>,
}

impl Drop for ScreenshotWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Start watching `folder`. Returns None if the folder doesn't exist or a
/// watcher couldn't be created. `on_file` runs once per new, stable image file.
pub fn start<F>(folder: PathBuf, on_file: F) -> Option<ScreenshotWatcher>
where
    F: Fn(PathBuf) + Send + Sync + 'static,
{
    if !folder.is_dir() {
        eprintln!("paste2ssh: screenshot folder not found: {}", folder.display());
        return None;
    }

    let known = Arc::new(Mutex::new(seed_existing(&folder)));
    let stop = Arc::new(AtomicBool::new(false));
    let on_file = Arc::new(on_file);

    let (events_tx, events_rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = events_tx.send(res);
    })
    .ok()?;
    watcher.watch(&folder, RecursiveMode::NonRecursive).ok()?;

    {
        let known = known.clone();
        let stop = stop.clone();
        let on_file = on_file.clone();
        thread::Builder::new()
            .name("paste2ssh-screens".into())
            .spawn(move || {
                while events_rx.recv().is_ok() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    for path in image_files(&folder) {
                        {
                            let mut k = known.lock().unwrap();
                            if k.contains(&path) {
                                continue;
                            }
                            k.insert(path.clone());
                        }
                        let on_file = on_file.clone();
                        let stop = stop.clone();
                        thread::spawn(move || {
                            if wait_until_stable(&path) && !stop.load(Ordering::Relaxed) {
                                on_file(path);
                            }
                        });
                    }
                }
            })
            .ok()?;
    }

    Some(ScreenshotWatcher {
        _watcher: watcher,
        stop,
    })
}

fn seed_existing(folder: &Path) -> HashSet<PathBuf> {
    image_files(folder).into_iter().collect()
}

fn image_files(folder: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let ext_ok = path
            .extension()
            .map(|e| ALLOWED_EXT.contains(&e.to_string_lossy().to_lowercase().as_str()))
            .unwrap_or(false);
        if !ext_ok {
            continue;
        }
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            files.push(path);
        }
    }
    files
}

/// Wait until the file size is stable (two equal positive samples) before we
/// treat it as fully written. Mirrors `ScreenshotWatcher.waitUntilStable`.
fn wait_until_stable(path: &Path) -> bool {
    let mut previous: i64 = -1;
    let mut stable = 0;

    for _ in 0..12 {
        let size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(-1);
        if size > 0 && size == previous {
            stable += 1;
            if stable >= 2 {
                return true;
            }
        } else {
            stable = 0;
        }
        previous = size;
        thread::sleep(Duration::from_millis(250));
    }

    std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false)
}
