# Paste2SSH for Linux

Clipboard and screenshot images straight to your SSH host. Turn on Paste Mode,
copy an image (or take a screenshot), and Paste2SSH uploads it over `ssh`/`scp`
and replaces your clipboard with the remote path — ready to paste into Claude
Code, a terminal, or anywhere else.

This is the **Linux port** of the macOS Paste2SSH app, built with **Tauri**
(Rust core + system webview). It reuses the same behavior and visual design.

## Features

- **Clipboard image upload** — copy any image while Paste Mode is on; the remote
  path is copied back to your clipboard automatically.
- **Screenshot watching** — every screenshot saved to your screenshot folder is
  uploaded while Paste Mode is on (deduped against the clipboard so one capture
  uploads exactly once).
- **Drag & drop any file** onto the window — uploads with the original filename.
- **Host picker** from `~/.ssh/config`, plus in-app add/edit/delete of hosts.
- **Instant re-uploads** via SSH `ControlMaster` connection reuse.
- **System tray** with on/off, host selection, and copy-last-path.
- **Launch at login** (XDG autostart).

Uploads go to **`/tmp/paste2ssh`** on the remote by default — the OS clears
`/tmp` on reboot, so there is nothing to clean up.

## Runtime requirements

- `openssh-client` (`ssh` + `scp` on `PATH`).
- `libwebkit2gtk-4.1` (the webview; present on most modern desktops, otherwise
  one `apt`/`dnf` install).
- A clipboard backend: X11 works out of the box; on Wayland, image clipboard
  reads rely on `wlr-data-control` (wlroots compositors and recent GNOME). Where
  that isn't available, the **screenshot-folder watcher** still uploads captures.

## Build

Prerequisites: a Rust toolchain and the Tauri CLI.

```sh
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

# Tauri CLI + system deps (Debian/Ubuntu example)
cargo install tauri-cli --version "^2"
sudo apt install libwebkit2gtk-4.1-dev build-essential libssl-dev \
                 libayatana-appindicator3-dev librsvg2-dev openssh-client
```

Then, from the project root:

```sh
cargo tauri dev      # run locally with hot-reload
cargo tauri build    # produce an AppImage in src-tauri/target/release/bundle/appimage/
```

The AppImage is a single self-contained download — the Linux equivalent of the
macOS DMG.

## Project layout

```
src-tauri/
  src/
    main.rs        entry point
    lib.rs         Tauri commands + app wiring
    state.rs       central state, upload pipeline, watcher orchestration
    ssh.rs         ssh/scp engine (ControlMaster, scp -O fallback, error mapping)
    ssh_config.rs  ~/.ssh/config parsing + host add/edit/delete
    config.rs      settings (JSON at ~/.config/paste2ssh/settings.json)
    clipboard.rs   clipboard service (arboard) + 1s image poll
    watcher.rs     screenshot-folder watcher (inotify via notify)
    imageutil.rs   PNG encode + content-hash dedup
    tray.rs        system tray icon states + menu
    login_item.rs  XDG autostart .desktop
  tauri.conf.json  window + bundle config (AppImage)
ui/                HTML/CSS/JS frontend (4 pages + slide-in panels)
```

## Configuration

- Settings: `~/.config/paste2ssh/settings.json`
- Recent uploads: `~/.config/paste2ssh/recent.json`
- Control sockets: `~/.cache/paste2ssh/cm/`
- Autostart: `~/.config/autostart/paste2ssh.desktop`

## Tests

```sh
cd src-tauri && cargo test --lib
```

Includes an end-to-end upload test that drives the real `SshUploader` against
local stub `ssh`/`scp` scripts (overridable via `P2SS_SSH_BIN` / `P2SS_SCP_BIN`).

## Not in v1

- In-app auto-update (re-download the AppImage to update).
- AppImage signing.
