//! Clipboard service. A single dedicated thread owns one `arboard::Clipboard`
//! for the app's lifetime (keeping ownership so a copied remote path persists),
//! handles `SetText` commands, and — when watching is enabled — polls once a
//! second for a new image (port of `ClipboardWatcher.swift`). The pixel hash is
//! passed to the callback for cross-source dedup with the screenshot watcher.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use crate::imageutil;

enum Command {
    SetText(String),
    SetWatching(bool),
    Stop,
}

#[derive(Clone)]
pub struct ClipboardHandle {
    tx: Sender<Command>,
}

impl ClipboardHandle {
    pub fn set_text(&self, text: String) {
        let _ = self.tx.send(Command::SetText(text));
    }

    pub fn set_watching(&self, on: bool) {
        let _ = self.tx.send(Command::SetWatching(on));
    }

    #[allow(dead_code)]
    pub fn stop(&self) {
        let _ = self.tx.send(Command::Stop);
    }
}

/// Start the clipboard service. `on_image` is called with (png_bytes, pixel_hash)
/// when a new image appears while watching.
pub fn start<F>(on_image: F) -> ClipboardHandle
where
    F: Fn(Vec<u8>, String) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Command>();

    thread::Builder::new()
        .name("paste2ssh-clipboard".into())
        .spawn(move || {
            let mut clipboard = arboard::Clipboard::new().ok();
            if clipboard.is_none() {
                eprintln!("paste2ssh: clipboard init failed; will retry");
            }
            let mut watching = false;
            let mut last_hash: Option<String> = None;
            let poll = Duration::from_secs(1);

            loop {
                match rx.recv_timeout(poll) {
                    Ok(Command::Stop) => break,
                    Ok(Command::SetText(text)) => {
                        if clipboard.is_none() {
                            clipboard = arboard::Clipboard::new().ok();
                        }
                        if let Some(cb) = clipboard.as_mut() {
                            if let Err(e) = cb.set_text(text) {
                                eprintln!("paste2ssh: clipboard set_text failed: {e}");
                            }
                        }
                    }
                    Ok(Command::SetWatching(on)) => {
                        watching = on;
                        last_hash = if on {
                            // Seed with the current clipboard so we don't upload a
                            // pre-existing image the moment Paste Mode turns on.
                            current_image_hash(clipboard.as_mut())
                        } else {
                            None
                        };
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if !watching {
                            continue;
                        }
                        if clipboard.is_none() {
                            clipboard = arboard::Clipboard::new().ok();
                        }
                        let Some(cb) = clipboard.as_mut() else { continue };
                        let Ok(img) = cb.get_image() else { continue };
                        let (w, h) = (img.width as u32, img.height as u32);
                        let hash = imageutil::pixel_hash_rgba(w, h, &img.bytes);
                        if last_hash.as_deref() == Some(hash.as_str()) {
                            continue;
                        }
                        last_hash = Some(hash.clone());
                        match imageutil::encode_png_from_rgba(w, h, &img.bytes) {
                            Ok(png) => on_image(png, hash),
                            Err(e) => eprintln!("paste2ssh: png encode failed: {e}"),
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .expect("spawn clipboard thread");

    ClipboardHandle { tx }
}

fn current_image_hash(cb: Option<&mut arboard::Clipboard>) -> Option<String> {
    let cb = cb?;
    let img = cb.get_image().ok()?;
    Some(imageutil::pixel_hash_rgba(
        img.width as u32,
        img.height as u32,
        &img.bytes,
    ))
}
