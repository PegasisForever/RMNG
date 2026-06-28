//! Proxmox orchestration over SSH — the Rust port of `proxmox.server.ts`.
//!
//! Each operation ships a self-contained bash script to the node over
//! `ssh <target> bash -s -- <args>` and parses its stdout line protocol:
//!   `P <step> <message...>`  progress tick
//!   `RESULT <fields...>`     final result
//! Interpolated args are validated and single-quoted before reaching the shell.

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use wire::{AppConfig, ProxmoxConfig};

const CLONE_SCRIPT: &str = include_str!("../scripts/clone.sh");
const DELETE_SCRIPT: &str = include_str!("../scripts/delete.sh");
const BOOTSTRAP_SCRIPT: &str = include_str!("../scripts/bootstrap.sh");
const PROVISION_SCRIPT: &str = include_str!("../scripts/provision-clone.sh");
const REDEPLOY_SCRIPT: &str = include_str!("../scripts/redeploy.sh");
const APPLY_MONITORS_SCRIPT: &str = include_str!("../scripts/apply-monitors.sh");

/// The monitor layout as the clone-daemon's `RMNG_MONITORS` env: CSV of `WxH+X+Y[*]`
/// (position in the unified desktop, `*` = primary).
pub fn monitors_csv(cfg: &AppConfig) -> String {
    cfg.effective_monitors()
        .iter()
        .map(|m| format!("{}x{}+{}+{}{}", m.width, m.height, m.x, m.y, if m.primary { "*" } else { "" }))
        .collect::<Vec<_>>()
        .join(",")
}

/// `scp` a local file to the node. Best-effort delivery for binaries too big to
/// pass as a command-line arg.
async fn scp_to_node(ssh_target: &str, local: &str, remote: &str) -> Result<()> {
    let dest = format!("{ssh_target}:{remote}");
    let status = Command::new("scp")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15", local, &dest])
        .status()
        .await
        .map_err(|e| anyhow!("spawning scp: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("scp {local} -> {dest} failed (exit {:?})", status.code())
    }
}

/// Stage an **embedded** binary on the node for bootstrap.sh to push into the new
/// CT. Decompresses `<name>.gz` from the control-server, writes it to a temp file,
/// scp's it to `node_path`. Returns the node path if delivered, else `""`.
/// (Falls back to a dev on-disk path via `RMNG_<NAME>_BIN` if not embedded.)
async fn stage_binary(ssh_target: &str, name: &str, node_path: &str) -> Result<String> {
    let bytes = match crate::embed::embedded_binary(name) {
        Some(b) => b,
        None => {
            // Dev fallback: an on-disk binary (e.g. cargo target) via env override.
            let env = format!("RMNG_{}_BIN", name.to_uppercase().replace('-', "_"));
            match std::env::var(&env) {
                Ok(p) if std::path::Path::new(&p).exists() => {
                    scp_to_node(ssh_target, &p, node_path).await?;
                    return Ok(node_path.to_string());
                }
                _ => return Ok(String::new()), // not embedded, no override → skip
            }
        }
    };
    let tmp = std::env::temp_dir().join(format!("rmng-{name}-{}", std::process::id()));
    std::fs::write(&tmp, &bytes).with_context(|| format!("writing {name} to temp"))?;
    let r = scp_to_node(ssh_target, &tmp.to_string_lossy(), node_path).await;
    let _ = std::fs::remove_file(&tmp);
    r?;
    Ok(node_path.to_string())
}

pub fn is_dns_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// POSIX single-quote escaping.
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Run a script on the node, streaming progress; returns the `RESULT` fields.
pub async fn run_remote(
    ssh_target: &str,
    script: &str,
    args: &[&str],
    mut on_progress: impl FnMut(&str, &str),
) -> Result<Vec<String>> {
    if ssh_target.is_empty() {
        bail!("config proxmox.ssh is not set; cannot reach the node");
    }
    let remote_cmd =
        format!("bash -s -- {}", args.iter().map(|a| sq(a)).collect::<Vec<_>>().join(" "));
    let mut child = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15", ssh_target, &remote_cmd])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("spawning ssh: {e}"))?;

    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(script.as_bytes()).await?;
    drop(stdin); // EOF so `bash -s` runs

    let stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let stderr_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });

    let mut result: Option<Vec<String>> = None;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        if let Some(rest) = line.strip_prefix("P ") {
            let (step, msg) = rest.split_once(' ').unwrap_or((rest, ""));
            on_progress(step, msg);
        } else if let Some(rest) = line.strip_prefix("RESULT ") {
            result = Some(rest.trim().split_whitespace().map(str::to_string).collect());
        }
    }

    let status = child.wait().await?;
    let err = stderr_task.await.unwrap_or_default();
    if status.success() {
        result.ok_or_else(|| anyhow!("remote command produced no RESULT line"))
    } else {
        let tail: Vec<&str> = err.trim().lines().rev().take(6).collect();
        let tail: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        bail!(
            "remote command failed (exit {:?}){}",
            status.code(),
            if tail.is_empty() { String::new() } else { format!(":\n{tail}") }
        )
    }
}

/// CoW-clone a template CT. Returns `(ctid, ip)`.
pub async fn clone_ct(
    cfg: &ProxmoxConfig,
    source_id: &str,
    new_hostname: &str,
    username: &str,
    env: &[wire::EnvVar],
    on_progress: impl FnMut(&str, &str),
) -> Result<(u32, String)> {
    if !is_dns_label(source_id) {
        bail!("source id must be a DNS label");
    }
    if !is_dns_label(new_hostname) {
        bail!("new hostname must be a DNS label");
    }
    // Chosen env preset → base64 KEY=VALUE lines that clone.sh writes into the clone's
    // ~/.config/environment.d/30-rmng-preset.conf BEFORE first boot (no session restart).
    let env_content: String =
        env.iter().filter(|v| !v.key.is_empty()).map(|v| format!("{}={}\n", v.key, v.value)).collect();
    let env_b64 = b64_encode(env_content.as_bytes());
    let result = run_remote(
        &cfg.ssh,
        CLONE_SCRIPT,
        &[source_id, new_hostname, &cfg.mac_prefix, username, &env_b64],
        on_progress,
    )
    .await?;
    let ctid: u32 = result
        .first()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("clone returned an unexpected result: {result:?}"))?;
    let ip = result.get(1).cloned().ok_or_else(|| anyhow!("clone returned no IP: {result:?}"))?;
    Ok((ctid, ip))
}

/// Build a template/clone CT **from a base image** (the from-zero path): create +
/// render passthrough + headless-GNOME provisioning, all on the node. Returns
/// `(ctid, ip)`. The in-CT provisioning script is shipped base64 to the node.
pub async fn bootstrap_template(
    cfg: &AppConfig,
    hostname: &str,
    on_progress: impl FnMut(&str, &str),
) -> Result<(u32, String)> {
    if !is_dns_label(hostname) {
        bail!("hostname must be a DNS label");
    }
    let prov_b64 = b64_encode(PROVISION_SCRIPT.as_bytes());
    // Stage the embedded clone-daemon + agent-wrapper binaries on the node so
    // bootstrap.sh can push them into the new CT (provision-clone.sh installs them).
    let ssh = &cfg.proxmox.ssh;
    let cd_arg = stage_binary(ssh, "clone-daemon", &format!("/tmp/clone-daemon-{hostname}")).await?;
    let aw_arg = stage_binary(ssh, "agent-wrapper", &format!("/tmp/agent-wrapper-{hostname}")).await?;
    // Patched gnome-shell deb (shell-01 hide-indicator + shell-03 enable-Eval). Staged
    // like the binaries; bootstrap.sh pushes it in and provision-clone.sh installs it.
    // Empty if not embedded → provision falls back to stock shell (no window-mgmt MCP).
    let shell_arg = stage_binary(ssh, "gnome-shell-deb", &format!("/tmp/gnome-shell-{hostname}.deb")).await?;
    // Monitor layout → the clone-daemon's `RMNG_MONITORS` env. One virtual monitor per
    // entry; falls back to a single primary 1080p.
    let mons = monitors_csv(cfg);
    let result = run_remote(
        ssh,
        BOOTSTRAP_SCRIPT,
        &[hostname, &cfg.template.base_image, "local-lvm", "vmbr0", &prov_b64, &cd_arg, &aw_arg, &mons, &shell_arg],
        on_progress,
    )
    .await?;
    let ctid: u32 = result
        .first()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("bootstrap returned an unexpected result: {result:?}"))?;
    let ip = result.get(1).cloned().ok_or_else(|| anyhow!("bootstrap returned no IP: {result:?}"))?;
    Ok((ctid, ip))
}

fn b64_encode(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { A[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Apply the configured monitor layout to a RUNNING clone without reprovisioning: rewrite
/// its `RMNG_MONITORS` env + dummy mode specs and restart its headless GNOME + daemon
/// (the daemon re-creates the virtual monitors + applies positions/primary at startup).
pub async fn apply_monitors(
    cfg: &AppConfig,
    ctid: u32,
    username: &str,
    on_progress: impl FnMut(&str, &str),
) -> Result<()> {
    run_remote(
        &cfg.proxmox.ssh,
        APPLY_MONITORS_SCRIPT,
        &[&ctid.to_string(), username, &monitors_csv(cfg)],
        on_progress,
    )
    .await?;
    Ok(())
}

/// Destroy a CT by id.
pub async fn delete_ct(
    cfg: &ProxmoxConfig,
    ctid: u32,
    on_progress: impl FnMut(&str, &str),
) -> Result<()> {
    run_remote(&cfg.ssh, DELETE_SCRIPT, &[&ctid.to_string()], on_progress).await?;
    Ok(())
}

/// Hot-swap a running clone's `clone-daemon` (+ `agent-wrapper` unless `daemon_only`)
/// binaries WITHOUT reprovisioning: decompress the control-server's embedded copies,
/// `scp` them to the node, then `pct push` them into the CT + restart its user units.
/// The daemon reconnects to the media socket on restart; the agent-wrapper drops its
/// current Claude session, so prefer `daemon_only` while an agent turn is in flight.
pub async fn redeploy_clone(
    cfg: &AppConfig,
    ctid: u32,
    username: &str,
    daemon_only: bool,
    on_progress: impl FnMut(&str, &str),
) -> Result<()> {
    let ssh = &cfg.proxmox.ssh;
    let cd = stage_binary(ssh, "clone-daemon", &format!("/tmp/redeploy-cd-{ctid}")).await?;
    let aw = if daemon_only {
        "-".to_string()
    } else {
        stage_binary(ssh, "agent-wrapper", &format!("/tmp/redeploy-aw-{ctid}")).await?
    };
    run_remote(ssh, REDEPLOY_SCRIPT, &[&ctid.to_string(), username, &cd, &aw], on_progress).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_label_validation() {
        assert!(is_dns_label("pega-we-142"));
        assert!(is_dns_label("a"));
        assert!(!is_dns_label("UPPER"));
        assert!(!is_dns_label("-lead"));
        assert!(!is_dns_label("trail-"));
        assert!(!is_dns_label("has space"));
        assert!(!is_dns_label(""));
    }

    #[test]
    fn single_quote_escaping() {
        assert_eq!(sq("plain"), "'plain'");
        assert_eq!(sq("a'b"), r"'a'\''b'");
    }
}
