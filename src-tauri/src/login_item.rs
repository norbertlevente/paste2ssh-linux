//! Launch-at-login via an XDG autostart .desktop file (replaces macOS
//! SMAppService). When running from an AppImage we point Exec at the AppImage
//! itself (the APPIMAGE env var) rather than the extracted temp binary.

use crate::paths;

pub fn is_enabled() -> bool {
    paths::autostart_file().exists()
}

pub fn set_enabled(on: bool) -> Result<(), String> {
    let path = paths::autostart_file();

    if on {
        let exec = executable_path()?;
        let content = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Paste2SSH\n\
             Comment=Clipboard/screenshot images straight to your SSH host\n\
             Exec={exec}\n\
             Icon=paste2ssh\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n"
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, content).map_err(|e| e.to_string())?;
    } else if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }

    Ok(())
}

fn executable_path() -> Result<String, String> {
    if let Ok(appimage) = std::env::var("APPIMAGE") {
        if !appimage.is_empty() {
            return Ok(appimage);
        }
    }
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}
