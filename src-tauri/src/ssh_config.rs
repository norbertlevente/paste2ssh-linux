//! Port of `SSHConfigHosts.swift` plus the host add/edit/delete block writing
//! from `AppState.swift`. Operates purely on ~/.ssh/config; the caller
//! (`state.rs`) applies the matching Settings changes (per-host remote dir,
//! selection) after a successful write.

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use serde::Serialize;

use crate::config::Settings;
use crate::paths;

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionDetails {
    pub alias: String,
    pub host_name: String,
    pub user: String,
    pub port: String,
    pub remote_dir: String,
}

/// Parsed `Host` aliases (wildcards skipped, deduped, case-insensitive sort).
pub fn load_hosts() -> Vec<String> {
    match std::fs::read_to_string(paths::ssh_config_file()) {
        Ok(text) => parse_host_aliases(&text),
        Err(_) => Vec::new(),
    }
}

/// Pure parser for the `Host` aliases in an ssh config (testable without I/O).
fn parse_host_aliases(text: &str) -> Vec<String> {
    let mut hosts: BTreeSet<String> = BTreeSet::new();
    for raw_line in text.split('\n') {
        // strip an inline comment, then trim
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if !line.to_lowercase().starts_with("host ") {
            continue;
        }
        for alias in line[5..].split_whitespace() {
            if alias.contains('*') || alias.contains('?') {
                continue;
            }
            hosts.insert(alias.to_string());
        }
    }

    let mut result: Vec<String> = hosts.into_iter().collect();
    result.sort_by_key(|h| h.to_lowercase());
    result
}

/// Detailed connection info for one host, for the add/edit form.
pub fn connection_details(settings: &Settings, host: &str) -> ConnectionDetails {
    let target = host.trim().to_string();
    let remote_dir = settings.remote_dir_for(&target);
    let mut details = ConnectionDetails {
        alias: target.clone(),
        host_name: String::new(),
        user: String::new(),
        port: String::new(),
        remote_dir,
    };

    let config_path = ensure_ssh_config();
    let text = match std::fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(_) => return details,
    };
    let lines = split_lines(&text);
    let Some(block) = host_block(&target, &lines) else {
        return details;
    };

    for line in block.lines.iter().skip(1) {
        let parts: Vec<&str> = line.trim().split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let key = parts[0].to_lowercase();
        let value = parts[1..].join(" ");
        match key.as_str() {
            "hostname" => details.host_name = value,
            "user" => details.user = value,
            "port" => details.port = value,
            _ => {}
        }
    }
    details
}

/// Add or edit a host block. On success returns the sanitized alias; the caller
/// applies the remote-dir/selection changes. On failure returns a message.
pub fn save_connection(
    original_host: Option<&str>,
    raw_alias: &str,
    raw_host_name: &str,
    raw_user: &str,
    raw_port: &str,
) -> Result<String, String> {
    let alias = sanitize_alias(raw_alias);
    let host_name = raw_host_name.trim().to_string();
    let user = raw_user.trim().to_string();
    let port = raw_port.trim().to_string();

    if alias.is_empty() || host_name.is_empty() {
        return Err("Name and server are required.".to_string());
    }
    let existing = load_hosts();
    if original_host != Some(alias.as_str()) && existing.iter().any(|h| h == &alias) {
        return Err(format!("A Host named {alias} already exists."));
    }
    if !port.is_empty() && port.parse::<u32>().is_err() {
        return Err("Port must be a number.".to_string());
    }

    let config_path = ensure_ssh_config();
    let new_block = ssh_config_block(&alias, &host_name, &user, &port, original_host);

    if let Some(original) = original_host {
        let text = std::fs::read_to_string(&config_path).unwrap_or_default();
        let mut lines = split_lines(&text);
        let Some(block) = host_block(original, &lines) else {
            return Err(format!("Could not find Host {original} in ~/.ssh/config."));
        };
        let replacement: Vec<String> = new_block.split('\n').map(|s| s.to_string()).collect();
        lines.splice(block.start..block.end, replacement);
        write_atomic(&config_path, &lines.join("\n"))?;
    } else {
        let mut text = std::fs::read_to_string(&config_path).unwrap_or_default();
        text.push('\n');
        text.push_str(&new_block);
        text.push('\n');
        write_atomic(&config_path, &text)?;
    }

    Ok(alias)
}

/// Remove a host block. On success returns nothing; the caller clears Settings.
pub fn delete_connection(host: &str) -> Result<(), String> {
    let target = host.trim().to_string();
    if target.is_empty() {
        return Err("No host selected.".to_string());
    }

    let config_path = ensure_ssh_config();
    let text = std::fs::read_to_string(&config_path).map_err(|_| "Could not read ~/.ssh/config.".to_string())?;
    let lines = split_lines(&text);

    let mut output: Vec<String> = Vec::new();
    let mut removed = false;
    let mut skipping = false;

    for line in &lines {
        let parts: Vec<String> = line.trim().split_whitespace().map(|s| s.to_string()).collect();
        let is_host_line = parts.first().map(|p| p.to_lowercase()) == Some("host".to_string());

        if is_host_line {
            if parts.iter().skip(1).any(|p| p == &target) {
                skipping = true;
                removed = true;
                continue;
            } else {
                skipping = false;
            }
        }

        if !skipping {
            output.push(line.clone());
        }
    }

    if !removed {
        return Err(format!("Could not find Host {target} in ~/.ssh/config."));
    }

    write_atomic(&config_path, &output.join("\n"))
}

// --- helpers -----------------------------------------------------------------

/// Ensure ~/.ssh (0700) and an empty ~/.ssh/config exist; return the path.
pub fn ensure_ssh_config() -> PathBuf {
    let config_path = paths::ssh_config_file();
    if let Some(ssh_dir) = config_path.parent() {
        let _ = std::fs::create_dir_all(ssh_dir);
        let _ = std::fs::set_permissions(ssh_dir, std::fs::Permissions::from_mode(0o700));
    }
    if !config_path.exists() {
        let _ = std::fs::write(&config_path, b"");
    }
    config_path
}

struct HostBlock {
    start: usize,
    end: usize,
    lines: Vec<String>,
}

/// Find the line range of the `Host <host>` block (up to the next Host line).
fn host_block(host: &str, lines: &[String]) -> Option<HostBlock> {
    let mut start: Option<usize> = None;
    let mut end = lines.len();

    for (index, line) in lines.iter().enumerate() {
        let parts: Vec<&str> = line.trim().split_whitespace().collect();
        let is_host_line = parts.first().map(|p| p.to_lowercase()) == Some("host".to_string());

        if is_host_line {
            if let Some(start_index) = start {
                end = index;
                return Some(HostBlock {
                    start: start_index,
                    end,
                    lines: lines[start_index..end].to_vec(),
                });
            }
            if parts.iter().skip(1).any(|p| *p == host) {
                start = Some(index);
            }
        }
    }

    start.map(|start_index| HostBlock {
        start: start_index,
        end,
        lines: lines[start_index..end].to_vec(),
    })
}

/// Build a Host block, preserving any keys other than HostName/User/Port from
/// the original block when editing.
fn ssh_config_block(
    alias: &str,
    host_name: &str,
    user: &str,
    port: &str,
    original_host: Option<&str>,
) -> String {
    let mut preserved: Vec<String> = Vec::new();
    if let Some(original) = original_host {
        if let Ok(text) = std::fs::read_to_string(ensure_ssh_config()) {
            let lines = split_lines(&text);
            if let Some(block) = host_block(original, &lines) {
                for line in block.lines.iter().skip(1) {
                    let key = line
                        .trim()
                        .split_whitespace()
                        .next()
                        .map(|s| s.to_lowercase());
                    match key.as_deref() {
                        Some("hostname") | Some("user") | Some("port") | None => {}
                        Some(_) => preserved.push(line.clone()),
                    }
                }
            }
        }
    }

    let mut lines = vec![format!("Host {alias}"), format!("    HostName {host_name}")];
    if !user.is_empty() {
        lines.push(format!("    User {user}"));
    }
    if !port.is_empty() {
        lines.push(format!("    Port {port}"));
    }
    lines.extend(preserved);
    lines.join("\n")
}

fn sanitize_alias(value: &str) -> String {
    let mapped: String = value
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    mapped.trim_matches(|c| c == '-' || c == '.').to_string()
}

fn split_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect()
}

fn write_atomic(path: &std::path::Path, contents: &str) -> Result<(), String> {
    let tmp = path.with_extension("config.tmp");
    std::fs::write(&tmp, contents).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hosts_skipping_wildcards_and_comments() {
        let cfg = "\
# a comment
Host prod staging
    HostName 10.0.0.1
Host *.internal
    User root
Host beta # trailing comment
Host *
    ForwardAgent yes
";
        let hosts = parse_host_aliases(cfg);
        assert_eq!(hosts, vec!["beta", "prod", "staging"]);
    }

    #[test]
    fn block_isolates_one_host() {
        let lines: Vec<String> = "\
Host a
    HostName 1.1.1.1
Host b
    HostName 2.2.2.2
    Port 2222"
            .split('\n')
            .map(|s| s.to_string())
            .collect();
        let block = host_block("b", &lines).expect("found b");
        assert_eq!(block.lines[0].trim(), "Host b");
        assert!(block.lines.iter().any(|l| l.contains("2.2.2.2")));
        assert!(!block.lines.iter().any(|l| l.contains("1.1.1.1")));
    }

    #[test]
    fn alias_sanitization() {
        assert_eq!(sanitize_alias("  my host! "), "my-host");
        assert_eq!(sanitize_alias("ok.name_1"), "ok.name_1");
    }
}
