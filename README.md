# Paste2SSH for Linux

**Clipboard and screenshot images straight to your SSH host.** Turn on Paste
Mode, copy an image or take a screenshot, and Paste2SSH uploads it over
`ssh`/`scp` and replaces your clipboard with the remote path — ready to paste
into Claude Code, a terminal, or anywhere else.

This is the Linux port of the [macOS Paste2SSH](https://paste2ssh.com) app,
built with Tauri (Rust core + native webview). Same workflow, same look.

> **Status: alpha.** It builds, tests, and runs on Linux CI (real `ssh`/`scp`
> upload + headless boot verified on every commit). Feedback very welcome.

## Download

Grab the latest AppImage — a single self-contained file that runs on most
distros:

**→ [Download Paste2SSH-x86_64.AppImage](https://github.com/norbertlevente/paste2ssh-linux/releases/latest/download/Paste2SSH-x86_64.AppImage)**

```bash
chmod +x Paste2SSH-x86_64.AppImage
./Paste2SSH-x86_64.AppImage
```

(Or grab it from the [Releases page](https://github.com/norbertlevente/paste2ssh-linux/releases/latest).)

### Requirements

- **`ssh` + `scp`** — `openssh-client` (almost always already installed).
- **`libwebkit2gtk-4.1`** — the webview. Present on most modern desktops; if not:
  - Debian/Ubuntu: `sudo apt install libwebkit2gtk-4.1-0`
  - Fedora: `sudo dnf install webkit2gtk4.1`
  - Arch: `sudo pacman -S webkit2gtk-4.1`
- `libfuse2` if your distro doesn't ship FUSE (needed to run any AppImage), or
  run with `./Paste2SSH-x86_64.AppImage --appimage-extract-and-run`.

## How it works

1. Add or pick an SSH host (read from your `~/.ssh/config`).
2. Hit the power button to turn on **Paste Mode**.
3. Copy an image or take a screenshot → it uploads, and the **remote path is
   copied to your clipboard**. Paste it anywhere.

Uploads go to **`/tmp/paste2ssh`** on the remote by default, which the OS clears
on reboot — so there's nothing to clean up.

### Features

- **Clipboard image upload** — copy an image while Paste Mode is on.
- **Screenshot watching** — every screenshot saved to your screenshot folder is
  uploaded while Paste Mode is on (deduped against the clipboard so one capture
  uploads exactly once).
- **Drag & drop any file** onto the window — uploads with the original filename,
  even with Paste Mode off.
- **Host management** — pick from `~/.ssh/config`, or add/edit/delete hosts and
  per-host remote folders in-app.
- **Instant re-uploads** via SSH `ControlMaster` connection reuse.
- **System tray** quick-toggle + host switch + copy-last-path (where your
  desktop shows tray icons).
- **Launch at login** (XDG autostart).

## Alpha notes

- **Background operation:** **minimize** the window to keep Paste Mode running in
  the background; **closing quits** the app. The tray (where shown) is a bonus
  quick-toggle.
- **GNOME tray:** GNOME hides StatusNotifierItem tray icons by default — install
  the *AppIndicator* extension to see Paste2SSH's tray icon. Everything works
  without it; you just use the window/taskbar instead.
- **Wayland clipboard:** copying images from the clipboard relies on
  `wlr-data-control` (works on wlroots compositors and recent GNOME). On X11 it
  always works. Where clipboard reads aren't available, the **screenshot-folder
  watcher** still uploads your captures.

## Differences from the macOS app

- Default remote dir is **`/tmp/paste2ssh`** (vs `~/.paste2ssh`), so there's no
  remote cleanup feature — the OS clears `/tmp`.
- **No in-app auto-update yet** — re-download the AppImage to update.

## Configuration

- Settings: `~/.config/paste2ssh/settings.json`
- Recent uploads: `~/.config/paste2ssh/recent.json`
- Control sockets: `~/.cache/paste2ssh/cm/`
- Autostart: `~/.config/autostart/paste2ssh.desktop`

Hosts live in your standard `~/.ssh/config`.

## Build from source

```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

# Tauri CLI + build deps (Debian/Ubuntu)
cargo install tauri-cli --version "^2" --locked
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
                 librsvg2-dev libgtk-3-dev build-essential libssl-dev \
                 openssh-client

cargo tauri dev      # run locally
cargo tauri build    # build the AppImage
```

Tests (incl. a real localhost `ssh`/`scp` upload in CI):

```bash
cd src-tauri && cargo test --lib
```

## License

© Norbert Levente Kiss. See the main project at [paste2ssh.com](https://paste2ssh.com).
