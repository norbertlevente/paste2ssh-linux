//! Port of `SSHUploader.swift`. Spawns the system `/usr/bin/ssh` and
//! `/usr/bin/scp`, reuses connections via ControlMaster, and maps raw stderr
//! into friendly messages. The remote-cleanup/cron logic is intentionally
//! dropped: the Linux app uploads to `/tmp`, which the OS clears on reboot.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use sha2::{Digest, Sha256};
use wait_timeout::ChildExt;

use crate::config::Settings;
use crate::paths;

const SSH_BIN: &str = "/usr/bin/ssh";
const SCP_BIN: &str = "/usr/bin/scp";

/// Resolve the ssh binary, allowing an override (used by tests; harmless in prod).
fn ssh_bin() -> String {
    std::env::var("P2SS_SSH_BIN").unwrap_or_else(|_| SSH_BIN.to_string())
}

/// Resolve the scp binary, allowing an override (used by tests; harmless in prod).
fn scp_bin() -> String {
    std::env::var("P2SS_SCP_BIN").unwrap_or_else(|_| SCP_BIN.to_string())
}

pub struct UploadResult {
    pub remote_path: String,
}

struct ProcessResult {
    status: i32,
    stdout: String,
    stderr: String,
}

#[derive(Default)]
struct Caches {
    cached_home: HashMap<String, String>,
    ensured_dirs: HashSet<String>,
}

pub struct SshUploader {
    caches: Mutex<Caches>,
}

impl SshUploader {
    pub fn new() -> Self {
        SshUploader {
            caches: Mutex::new(Caches::default()),
        }
    }

    /// Upload a local file; returns the remote path on success or a friendly
    /// error message on failure.
    pub fn upload(
        &self,
        local_path: &Path,
        filename: &str,
        settings: &Settings,
    ) -> Result<UploadResult, String> {
        if settings.normalized_host().is_empty() {
            return Err("Enter an SSH host in Settings first.".to_string());
        }

        let remote_dir = self.resolve_remote_dir(settings)?;
        let ensure_key = format!("{}|{}", settings.display_target(), remote_dir);

        let already_ensured = {
            let caches = self.caches.lock().unwrap();
            caches.ensured_dirs.contains(&ensure_key)
        };
        if !already_ensured {
            self.ensure_dir(&remote_dir, settings)?;
            self.caches.lock().unwrap().ensured_dirs.insert(ensure_key.clone());
        }

        let remote_path = join_remote(&remote_dir, filename);
        if let Err(err) = self.scp(local_path, &remote_path, settings) {
            // The remote dir may have been removed mid-session; re-verify next time.
            self.caches.lock().unwrap().ensured_dirs.remove(&ensure_key);
            return Err(err);
        }

        Ok(UploadResult { remote_path })
    }

    /// Probe the connection and pre-create the remote dir. Returns a
    /// human-readable status string ("Connection OK." or an error).
    pub fn test_connection(&self, settings: &Settings) -> String {
        if settings.normalized_host().is_empty() {
            return "Enter an SSH host first.".to_string();
        }

        let mut args = self.ssh_base_args(settings);
        args.push(settings.host_target());
        args.push("--".to_string());
        args.push("true".to_string());

        match self.run(SSH_BIN, &args, Duration::from_secs(20)) {
            Ok(result) if result.status == 0 => match self.resolve_remote_dir(settings) {
                Ok(remote_dir) => {
                    if let Err(err) = self.ensure_dir(&remote_dir, settings) {
                        return err;
                    }
                    self.caches
                        .lock()
                        .unwrap()
                        .ensured_dirs
                        .insert(format!("{}|{}", settings.display_target(), remote_dir));
                    "Connection OK.".to_string()
                }
                Err(err) => err,
            },
            Ok(result) => map_error(result.status, &combined_output(&result)),
            Err(err) => format!("Could not start ssh: {err}"),
        }
    }

    /// Close a host's multiplexed master connection (best-effort) and forget its
    /// "dir ensured" cache. Called when switching hosts or turning off.
    pub fn close_master(&self, settings: &Settings) {
        if settings.normalized_host().is_empty() {
            return;
        }
        let prefix = format!("{}|", settings.display_target());
        {
            let mut caches = self.caches.lock().unwrap();
            caches.ensured_dirs.retain(|k| !k.starts_with(&prefix));
        }
        let mut args = self.ssh_base_args(settings);
        args.push("-O".to_string());
        args.push("exit".to_string());
        args.push(settings.host_target());
        let _ = self.run(SSH_BIN, &args, Duration::from_secs(5));
    }

    fn resolve_remote_dir(&self, settings: &Settings) -> Result<String, String> {
        let configured = settings.effective_remote_dir().trim().to_string();
        if configured.starts_with('/') {
            return Ok(normalize_remote_path(&configured));
        }

        let cache_key = settings.display_target();
        let cached = {
            let caches = self.caches.lock().unwrap();
            caches.cached_home.get(&cache_key).cloned()
        };

        let home = match cached {
            Some(home) => home,
            None => {
                let mut args = self.ssh_base_args(settings);
                args.push(settings.host_target());
                args.push("--".to_string());
                args.push("printf".to_string());
                args.push("%s".to_string());
                args.push("$HOME".to_string());
                let result = self
                    .run(SSH_BIN, &args, Duration::from_secs(20))
                    .map_err(|e| format!("Could not start ssh: {e}"))?;
                if result.status != 0 {
                    return Err(map_error(result.status, &combined_output(&result)));
                }
                let resolved = result.stdout.trim().to_string();
                if resolved.is_empty() {
                    return Err("Could not resolve the remote home directory.".to_string());
                }
                self.caches
                    .lock()
                    .unwrap()
                    .cached_home
                    .insert(cache_key, resolved.clone());
                resolved
            }
        };

        let cleaned = if configured == "~" {
            String::new()
        } else if let Some(rest) = configured.strip_prefix("~/") {
            rest.to_string()
        } else {
            configured
        };
        let tail = if cleaned.is_empty() { "paste2ssh" } else { &cleaned };
        Ok(normalize_remote_path(&join_remote(&home, tail)))
    }

    fn ensure_dir(&self, remote_dir: &str, settings: &Settings) -> Result<(), String> {
        let command = format!("/bin/mkdir -p -- {}", shell_quote(remote_dir));
        let mut args = self.ssh_base_args(settings);
        args.push(settings.host_target());
        args.push("--".to_string());
        args.push(command);
        let result = self
            .run(&ssh_bin(), &args, Duration::from_secs(20))
            .map_err(|e| format!("Could not start ssh: {e}"))?;
        if result.status != 0 {
            return Err(map_error(result.status, &combined_output(&result)));
        }
        Ok(())
    }

    fn scp(&self, local_path: &Path, remote_path: &str, settings: &Settings) -> Result<(), String> {
        let remote_target = format!("{}:{}", settings.host_target(), remote_path);
        let local = local_path.to_string_lossy().into_owned();
        // Large screenshots over a slow uplink need more headroom than the quick
        // control commands; the connection itself is bounded by ConnectTimeout.
        let transfer_timeout = Duration::from_secs(120);

        let mut args = self.scp_base_args(settings);
        args.push(local.clone());
        args.push(remote_target.clone());
        let result = self
            .run(&scp_bin(), &args, transfer_timeout)
            .map_err(|e| format!("Could not start scp: {e}"))?;
        if result.status == 0 {
            return Ok(());
        }

        // Modern scp speaks SFTP. Some servers (dropbear, or no sftp-server
        // installed) only support the legacy transfer protocol, so retry with -O.
        let lower = combined_output(&result).to_lowercase();
        let looks_like_protocol = lower.contains("subsystem")
            || lower.contains("sftp")
            || lower.contains("protocol")
            || lower.contains("expand-path");
        if looks_like_protocol {
            let mut legacy_args = vec!["-O".to_string()];
            legacy_args.extend(self.scp_base_args(settings));
            legacy_args.push(local);
            legacy_args.push(remote_target);
            let legacy = self
                .run(&scp_bin(), &legacy_args, transfer_timeout)
                .map_err(|e| format!("Could not start scp: {e}"))?;
            if legacy.status == 0 {
                return Ok(());
            }
            return Err(map_error(legacy.status, &combined_output(&legacy)));
        }

        Err(map_error(result.status, &combined_output(&result)))
    }

    // --- connection multiplexing -------------------------------------------------

    /// Private, app-owned dir for SSH ControlMaster sockets. Re-created on every
    /// use (a no-op when it already exists): the cache dir can be purged at any
    /// time, and if we only made it once per launch a purge would make every
    /// ssh/scp fail the socket bind until restart. 0700 because ssh refuses
    /// world-writable ControlPath dirs.
    fn control_socket_directory(&self) -> Option<PathBuf> {
        let dir = paths::cache_dir().join("cm");
        std::fs::create_dir_all(&dir).ok()?;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        Some(dir)
    }

    /// Short 8-hex token of the target keeps the socket path well under the
    /// ~104-char unix-socket limit and identical across ssh + scp. An over-long
    /// ControlPath makes ssh hard-fail (exit 255), not fall back, so this is
    /// correctness, not tidiness.
    fn control_path(&self, settings: &Settings) -> Option<String> {
        let dir = self.control_socket_directory()?;
        let digest = Sha256::digest(settings.display_target().as_bytes());
        let token: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
        Some(dir.join(format!("cm-{token}")).to_string_lossy().into_owned())
    }

    fn control_args(&self, settings: &Settings) -> Vec<String> {
        match self.control_path(settings) {
            Some(path) => vec![
                "-o".to_string(),
                "ControlMaster=auto".to_string(),
                "-o".to_string(),
                format!("ControlPath={path}"),
                "-o".to_string(),
                "ControlPersist=120".to_string(),
            ],
            None => Vec::new(),
        }
    }

    fn ssh_base_args(&self, settings: &Settings) -> Vec<String> {
        let mut args = self.control_args(settings);
        args.extend([
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
        ]);
        if let Some(port) = settings.port {
            args.push("-p".to_string());
            args.push(port.to_string());
        }
        args
    }

    fn scp_base_args(&self, settings: &Settings) -> Vec<String> {
        let mut args = self.control_args(settings);
        args.extend([
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
        ]);
        if let Some(port) = settings.port {
            // scp uses uppercase -P for the port.
            args.push("-P".to_string());
            args.push(port.to_string());
        }
        args
    }

    fn run(&self, exe: &str, args: &[String], timeout: Duration) -> std::io::Result<ProcessResult> {
        let mut child = Command::new(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let (status, timed_out) = match child.wait_timeout(timeout)? {
            Some(status) => (status.code().unwrap_or(-1), false),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                (124, true)
            }
        };

        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut out) = child.stdout.take() {
            let _ = out.read_to_string(&mut stdout);
        }
        if let Some(mut err) = child.stderr.take() {
            let _ = err.read_to_string(&mut stderr);
        }

        Ok(ProcessResult {
            status: if timed_out { 124 } else { status },
            stdout,
            stderr,
        })
    }
}

impl Default for SshUploader {
    fn default() -> Self {
        Self::new()
    }
}

/// Port of `SSHUploader.mapError`: turn raw ssh/scp stderr into a friendly,
/// actionable message.
fn map_error(status: i32, stderr: &str) -> String {
    let lower = stderr.to_lowercase();
    let first_line = stderr
        .lines()
        .next()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "No error output.".to_string());

    if lower.contains("unix_listener")
        || lower.contains("mux_client")
        || lower.contains("control socket")
        || lower.contains("controlpath")
    {
        return "Local SSH connection setup failed. Please try again.".to_string();
    }
    if lower.contains("remote host identification has changed") || lower.contains("possible dns spoofing") {
        return "The host key changed since you last connected. If you trust this host, run ssh-keygen -R for it in a terminal, then reconnect.".to_string();
    }
    if lower.contains("host key verification failed") {
        return "Host key verification failed. Try connecting once in a terminal with ssh.".to_string();
    }
    if lower.contains("tailscale ssh requires an additional check") || lower.contains("login.tailscale.com") {
        if let Some(url) = extract_first_url(stderr) {
            return format!("Tailscale SSH needs approval: {url}");
        }
        return "Tailscale SSH needs approval. Connect to this host once in a terminal and approve the login request.".to_string();
    }
    if lower.contains("permission denied") || lower.contains("publickey") {
        return "SSH authentication failed. Confirm your key or agent works in a terminal.".to_string();
    }
    if lower.contains("connection refused") {
        return "Connection refused. Check the host and SSH port.".to_string();
    }
    if lower.contains("no route") || lower.contains("could not resolve") || lower.contains("name or service not known") {
        return "Could not reach that SSH host. Check the hostname or network.".to_string();
    }
    if lower.contains("timed out") || lower.contains("operation timed out") {
        return "SSH connection timed out.".to_string();
    }
    if status == 124 {
        return "SSH command timed out.".to_string();
    }
    if lower.contains("too many authentication failures") {
        return "The server rejected too many keys. Add IdentitiesOnly yes for this host in ~/.ssh/config.".to_string();
    }
    if lower.contains("no space left") {
        return "The remote disk is full. Free up space on the server, then try again.".to_string();
    }
    if lower.contains("kex_exchange_identification") || lower.contains("connection closed by remote host") {
        return "The server closed the connection. It may be blocking your IP (firewall/fail2ban) or not running SSH on this port.".to_string();
    }
    if lower.contains("missing operand") {
        return "Could not create the remote upload folder.".to_string();
    }
    if lower.contains("not writable") || lower.contains("read-only file system") || lower.contains("cannot create directory") {
        return "The remote upload directory is not writable.".to_string();
    }
    if lower.contains("subsystem") || lower.contains("protocol") || lower.contains("expand-path") {
        return "SFTP upload failed. Check that the server supports SFTP.".to_string();
    }
    if lower.contains("connection reset") || lower.contains("connection lost") || lower.contains("broken pipe") || lower.contains("client_loop") {
        return "The SSH connection dropped. Check your network or VPN (e.g. Tailscale), then try again.".to_string();
    }

    format!("SSH error (code {status}): {first_line}")
}

fn join_remote(base: &str, component: &str) -> String {
    let trimmed_base = base.trim_matches('/');
    let trimmed_component = component.trim_matches('/');
    if trimmed_component.is_empty() {
        format!("/{trimmed_base}")
    } else {
        format!("/{trimmed_base}/{trimmed_component}")
    }
}

fn normalize_remote_path(path: &str) -> String {
    let absolute = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in absolute.split('/') {
        if part == "." || part.is_empty() {
            continue;
        }
        if part == ".." {
            parts.pop();
        } else {
            parts.push(part);
        }
    }
    format!("/{}", parts.join("/"))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn combined_output(result: &ProcessResult) -> String {
    [result.stderr.as_str(), result.stdout.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Find the first http(s) URL in text, trimming common trailing punctuation.
fn extract_first_url(text: &str) -> Option<String> {
    let bytes = text;
    let idx = bytes.find("http://").or_else(|| bytes.find("https://"))?;
    let rest = &bytes[idx..];
    let end = rest
        .find(|c: char| c.is_whitespace())
        .unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches(['.', ',', ';', ')']);
    if url.is_empty() {
        None
    } else {
        Some(url.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_and_normalize() {
        assert_eq!(join_remote("/tmp/paste2ssh", "a.png"), "/tmp/paste2ssh/a.png");
        assert_eq!(join_remote("/tmp/paste2ssh/", "/a.png"), "/tmp/paste2ssh/a.png");
        assert_eq!(normalize_remote_path("/tmp/./paste2ssh/../paste2ssh"), "/tmp/paste2ssh");
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    }

    #[test]
    fn urls() {
        assert_eq!(
            extract_first_url("visit https://login.tailscale.com/a/b. now").as_deref(),
            Some("https://login.tailscale.com/a/b")
        );
    }

    /// End-to-end upload through stub ssh/scp: the stubs run the remote mkdir
    /// locally and `cp` the file into the "remote" dir. Verifies the full
    /// upload() path — dir-ensure, scp invocation, and remote-path building.
    #[test]
    fn upload_roundtrip_with_stubs() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!("p2ss-e2e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let remote = base.join("remote/paste2ssh");

        let ssh_stub = base.join("fake-ssh");
        fs::write(
            &ssh_stub,
            r#"#!/bin/sh
case " $* " in *" -O exit "*) exit 0 ;; esac
seen=0; cmd=""
for a in "$@"; do
  if [ "$seen" = "1" ]; then if [ -z "$cmd" ]; then cmd="$a"; else cmd="$cmd $a"; fi; fi
  [ "$a" = "--" ] && seen=1
done
if [ -n "$cmd" ]; then sh -c "$cmd"; exit $?; fi
exit 0
"#,
        )
        .unwrap();
        let scp_stub = base.join("fake-scp");
        fs::write(
            &scp_stub,
            r#"#!/bin/sh
prev=""; cur=""
for a in "$@"; do prev="$cur"; cur="$a"; done
dest="${cur#*:}"
mkdir -p "$(dirname "$dest")"
cp "$prev" "$dest"
"#,
        )
        .unwrap();
        fs::set_permissions(&ssh_stub, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&scp_stub, fs::Permissions::from_mode(0o755)).unwrap();

        std::env::set_var("P2SS_SSH_BIN", &ssh_stub);
        std::env::set_var("P2SS_SCP_BIN", &scp_stub);

        let local = base.join("shot.png");
        fs::write(&local, b"\x89PNG\r\n\x1a\nFAKEDATA").unwrap();

        let settings = Settings {
            host: "testhost".to_string(),
            remote_dir: remote.to_string_lossy().into_owned(),
            ..Settings::default()
        };

        let uploader = SshUploader::new();
        let result = uploader.upload(&local, "shot.png", &settings);

        std::env::remove_var("P2SS_SSH_BIN");
        std::env::remove_var("P2SS_SCP_BIN");

        let result = result.expect("upload should succeed");
        let expected = format!("{}/shot.png", remote.to_string_lossy());
        assert_eq!(result.remote_path, expected);
        let landed = fs::read(remote.join("shot.png")).expect("file should land remotely");
        assert_eq!(landed, b"\x89PNG\r\n\x1a\nFAKEDATA");

        let _ = fs::remove_dir_all(&base);
    }

    /// Real upload against a reachable host (CI starts a localhost sshd and sets
    /// P2SS_TEST_HOST). Ignored by default since it needs a live host. Because
    /// the host is localhost, the "remote" dir is local and we read it back.
    #[test]
    #[ignore = "requires a reachable SSH host via P2SS_TEST_HOST"]
    fn real_upload_localhost() {
        use std::fs;

        let host = match std::env::var("P2SS_TEST_HOST") {
            Ok(h) if !h.is_empty() => h,
            _ => {
                eprintln!("skip: P2SS_TEST_HOST not set");
                return;
            }
        };

        let dir = std::env::temp_dir().join("paste2ssh-ci");
        let _ = fs::remove_dir_all(&dir);
        let local = std::env::temp_dir().join("ci-shot.png");
        fs::write(&local, b"\x89PNG\r\n\x1a\nCIDATA").unwrap();

        let settings = Settings {
            host,
            remote_dir: dir.to_string_lossy().into_owned(),
            ..Settings::default()
        };

        let uploader = SshUploader::new();
        let result = uploader
            .upload(&local, "ci-shot.png", &settings)
            .expect("real upload should succeed");
        assert_eq!(
            result.remote_path,
            format!("{}/ci-shot.png", dir.to_string_lossy())
        );
        let landed = fs::read(dir.join("ci-shot.png")).expect("file should land");
        assert_eq!(landed, b"\x89PNG\r\n\x1a\nCIDATA");
    }
}
