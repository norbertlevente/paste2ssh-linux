//! Image encoding + content hashing shared by the clipboard and screenshot
//! watchers. The "pixel hash" is computed over decoded RGBA so that a screenshot
//! file and its clipboard copy (re-encoded differently) hash identically — that
//! is what lets us upload a single capture exactly once across both sources.

use std::collections::{HashSet, VecDeque};
use std::io::Cursor;
use std::path::Path;
use std::sync::Mutex;

use sha2::{Digest, Sha256};

/// Encode raw RGBA bytes to PNG.
pub fn encode_png_from_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let buffer: image::RgbaImage = image::ImageBuffer::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| "clipboard image had an unexpected size".to_string())?;
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Content hash of decoded RGBA pixels (+ dimensions).
pub fn pixel_hash_rgba(width: u32, height: u32, rgba: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(width.to_le_bytes());
    hasher.update(height.to_le_bytes());
    hasher.update(rgba);
    hex(hasher.finalize().as_slice())
}

/// Decode an image file and hash its pixels. Returns None if it can't be decoded.
pub fn pixel_hash_file(path: &Path) -> Option<String> {
    let image = image::open(path).ok()?;
    let rgba = image.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(pixel_hash_rgba(w, h, rgba.as_raw()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A small bounded set of recently-seen content hashes, shared between the
/// clipboard and screenshot watchers to suppress duplicate uploads of one
/// capture that lands in both places.
pub struct Dedup {
    inner: Mutex<DedupInner>,
    capacity: usize,
}

struct DedupInner {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl Dedup {
    pub fn new(capacity: usize) -> Self {
        Dedup {
            inner: Mutex::new(DedupInner {
                order: VecDeque::new(),
                set: HashSet::new(),
            }),
            capacity,
        }
    }

    /// Returns true if the hash was already seen (caller should skip); otherwise
    /// records it and returns false.
    pub fn seen_or_insert(&self, hash: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.set.contains(hash) {
            return true;
        }
        inner.set.insert(hash.to_string());
        inner.order.push_back(hash.to_string());
        while inner.order.len() > self.capacity {
            if let Some(old) = inner.order.pop_front() {
                inner.set.remove(&old);
            }
        }
        false
    }
}
