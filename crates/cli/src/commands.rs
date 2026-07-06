//! One handler per subcommand: a thin client call + rendering. Handlers return the
//! process exit code (0 ok, 3 operation failed, 4 timeout); transport/API errors
//! bubble up as `anyhow` errors and exit 1 from `main`.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use control_client::Client;
use serde_json::Value;
use wire::{ControlState, Operation, Provider};

use crate::args::{AccountCmd, DesktopCmd, ImageCmd, WaitArgs};
use crate::output::{human_size, pct, short_id, table};
use crate::wait::{WaitOutcome, wait_for_op};

fn emit_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

pub async fn ps(client: &Client, json: bool) -> Result<u8> {
    let st = client.state().await?;
    if json {
        emit_json(&st)?;
        return Ok(0);
    }
    let rows: Vec<Vec<String>> = st
        .hosts
        .iter()
        .map(|h| {
            let sel = if st.selected.as_deref() == Some(&h.id) {
                "*"
            } else {
                ""
            };
            vec![
                format!("{}{}", h.id, sel),
                h.monitor_state
                    .map(|m| format!("{m:?}").to_lowercase())
                    .unwrap_or_default(),
                h.agent_report
                    .map(|r| format!("{r:?}").to_lowercase())
                    .unwrap_or_default(),
                h.source.clone().unwrap_or_default(),
                h.claude_account_email
                    .clone()
                    .or(h.claude_selection.clone())
                    .unwrap_or_default(),
                h.codex_account_email
                    .clone()
                    .or(h.codex_selection.clone())
                    .unwrap_or_default(),
                h.state_note
                    .as_deref()
                    .map(|n| truncate(n, 48))
                    .unwrap_or_default(),
            ]
        })
        .collect();
    print!(
        "{}",
        table(
            &["ID", "STATE", "AGENT", "IMAGE", "CLAUDE", "CODEX", "NOTE"],
            &rows
        )
    );
    Ok(0)
}

pub async fn select(client: &Client, host: &str, json: bool) -> Result<u8> {
    let target = (host != "none").then_some(host);
    if let Some(id) = target {
        let st = client.state().await?;
        if !st.hosts.iter().any(|h| h.id == id) {
            bail!("unknown host '{id}' (see `rmng ps`)");
        }
    }
    let st = client.activate(target).await?;
    if json {
        emit_json(&st)?;
    } else {
        match target {
            Some(id) => println!("selected {id}"),
            None => println!("selection cleared"),
        }
    }
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
pub async fn clone(
    client: &Client,
    image: &str,
    hostname: &str,
    claude: Option<&str>,
    codex: Option<&str>,
    preset: Option<&str>,
    wait: &WaitArgs,
    json: bool,
) -> Result<u8> {
    let op = client
        .clone_host(image, hostname, claude, codex, preset)
        .await?;
    started(client, op, wait, json, "clone").await
}

pub async fn rm(client: &Client, host: &str, yes: bool, wait: &WaitArgs, json: bool) -> Result<u8> {
    if !yes {
        use std::io::{BufRead, IsTerminal, Write};
        if !std::io::stdin().is_terminal() {
            bail!("refusing to delete '{host}' non-interactively without --yes");
        }
        eprint!("delete host '{host}'? this destroys its container and volumes [y/N] ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            eprintln!("aborted");
            return Ok(1);
        }
    }
    let op = client.delete(host).await?;
    started(client, op, wait, json, "delete").await
}

pub async fn image(client: &Client, cmd: &ImageCmd, json: bool) -> Result<u8> {
    match cmd {
        ImageCmd::Ls => {
            let images = client.images().await?;
            if json {
                emit_json(&images)?;
                return Ok(0);
            }
            let rows: Vec<Vec<String>> = images
                .iter()
                .map(|i| {
                    vec![
                        i.reference.clone(),
                        short_id(&i.id),
                        human_size(i.size_bytes.max(0) as u64),
                        i.created_at.clone(),
                        if i.base { "yes".into() } else { "".into() },
                        i.created_from.clone().unwrap_or_default(),
                        i.in_use_by.join(","),
                    ]
                })
                .collect();
            print!(
                "{}",
                table(
                    &[
                        "REFERENCE",
                        "ID",
                        "SIZE",
                        "CREATED",
                        "BASE",
                        "FROM",
                        "IN-USE-BY"
                    ],
                    &rows
                )
            );
            Ok(0)
        }
        ImageCmd::Pull { reference, wait } => {
            let op = client.image_pull(reference.as_deref()).await?;
            started(client, op, wait, json, "pull").await
        }
        ImageCmd::Commit { host, name, wait } => {
            let op = client.image_commit(host, name).await?;
            started(client, op, wait, json, "commit").await
        }
        ImageCmd::Rm { reference } => {
            client.image_delete(reference).await?;
            if json {
                emit_json(&serde_json::json!({ "ok": true }))?;
            } else {
                println!("removed {reference}");
            }
            Ok(0)
        }
    }
}

pub async fn account(client: &Client, cmd: &AccountCmd, json: bool) -> Result<u8> {
    match cmd {
        AccountCmd::Ls { claude, codex } => {
            let st = client.state().await?;
            let is_codex = |a: &wire::ClaudeUsage| matches!(a.provider, Some(Provider::Codex));
            let accounts: Vec<_> = st
                .claude_accounts
                .iter()
                .filter(|a| {
                    if *claude {
                        !is_codex(a)
                    } else if *codex {
                        is_codex(a)
                    } else {
                        true
                    }
                })
                .cloned()
                .collect();
            if json {
                emit_json(&accounts)?;
                return Ok(0);
            }
            let rows: Vec<Vec<String>> = accounts
                .iter()
                .map(|a| {
                    vec![
                        a.email.clone(),
                        if is_codex(a) {
                            "codex".into()
                        } else {
                            "claude".into()
                        },
                        a.assignable
                            .map(|b| if b { "yes" } else { "no" }.to_string())
                            .unwrap_or_default(),
                        pct(&a.five_hour),
                        a.five_hour
                            .as_ref()
                            .and_then(|w| w.resets_at.clone())
                            .unwrap_or_default(),
                        pct(&a.seven_day),
                        a.error.clone().unwrap_or_default(),
                    ]
                })
                .collect();
            print!(
                "{}",
                table(
                    &[
                        "EMAIL",
                        "PROVIDER",
                        "ASSIGNABLE",
                        "5H",
                        "5H-RESETS",
                        "7D",
                        "ERROR"
                    ],
                    &rows
                )
            );
            // Groups come from config (redacted view), not state.
            if let Ok(cfg) = client.config().await {
                let fmt = |gs: &[wire::CloneGroup]| {
                    gs.iter()
                        .map(|g| format!("{}={}", g.name, g.accounts.join("+")))
                        .collect::<Vec<_>>()
                        .join("  ")
                };
                if !cfg.clone_groups.is_empty() {
                    println!("claude groups: {}", fmt(&cfg.clone_groups));
                }
                if !cfg.codex_groups.is_empty() {
                    println!("codex groups:  {}", fmt(&cfg.codex_groups));
                }
            }
            Ok(0)
        }
        AccountCmd::Swap {
            host,
            account,
            codex,
        } => {
            let reply = if *codex {
                client.codex_swap(host, account).await?
            } else {
                client.claude_swap(host, account).await?
            };
            if json {
                emit_json(&reply)?;
            } else {
                let email = reply
                    .get("account")
                    .and_then(|v| v.as_str())
                    .unwrap_or("none");
                let provider = if *codex { "codex" } else { "claude" };
                println!("swapped {host} {provider} → {email}");
            }
            Ok(0)
        }
    }
}

pub async fn ops(client: &Client, json: bool) -> Result<u8> {
    let st = client.state().await?;
    if json {
        emit_json(&st.operations)?;
        return Ok(0);
    }
    let rows: Vec<Vec<String>> = st
        .operations
        .iter()
        .map(|o| {
            vec![
                o.id.clone(),
                format!("{:?}", o.kind).to_lowercase(),
                o.target.clone(),
                format!("{:?}", o.status).to_lowercase(),
                o.step.clone(),
                format!("{:.0}%", o.pct),
                truncate(&o.message, 60),
            ]
        })
        .collect();
    print!(
        "{}",
        table(
            &["ID", "KIND", "TARGET", "STATUS", "STEP", "PCT", "MESSAGE"],
            &rows
        )
    );
    Ok(0)
}

pub async fn wait_cmd(client: &Client, op_id: &str, timeout: u64, json: bool) -> Result<u8> {
    settle(client, op_id, timeout, json).await
}

/// Shared tail for commands that start an operation: print it (or its id), then
/// `--wait` rides SSE to the terminal state.
async fn started(
    client: &Client,
    op: Operation,
    wait: &WaitArgs,
    json: bool,
    verb: &str,
) -> Result<u8> {
    if !wait.wait {
        if json {
            emit_json(&op)?;
        } else {
            println!(
                "{verb} started: op {} target {} (follow with `rmng wait {}`)",
                op.id, op.target, op.id
            );
        }
        return Ok(0);
    }
    if !json {
        eprintln!("{verb} started: op {} target {}", op.id, op.target);
    }
    settle(client, &op.id, wait.timeout, json).await
}

async fn settle(client: &Client, op_id: &str, timeout: u64, json: bool) -> Result<u8> {
    match wait_for_op(client, op_id, timeout).await? {
        WaitOutcome::Done(op) => {
            if json {
                emit_json(&op)?;
            } else {
                println!("done: {} ({})", op.target, op.message);
            }
            Ok(0)
        }
        WaitOutcome::Failed(op) => {
            if json {
                emit_json(&op)?;
            }
            eprintln!("operation failed: {}", op.message);
            Ok(3)
        }
        WaitOutcome::Vanished { ever_seen } => {
            if ever_seen {
                eprintln!(
                    "warning: op {op_id} disappeared without a terminal frame (finished ops are pruned seconds after settling — this is almost always the Done prune)"
                );
            } else {
                eprintln!(
                    "warning: op {op_id} not present in state (already finished and pruned, or never existed)"
                );
            }
            Ok(0)
        }
        WaitOutcome::TimedOut => {
            eprintln!(
                "timed out after {timeout}s waiting for op {op_id} (it may still be running — check `rmng ops`)"
            );
            Ok(4)
        }
    }
}

/// The copy-paste one-liner: inline `-J` jump through the bastion, terminating at the
/// clone's own sshd. `accept-new` makes the first connect prompt-free (host keys are stable).
pub fn build_ssh_command(public_host: &str, bastion_port: u16, clone_id: &str) -> String {
    format!(
        "ssh -J rmng@{public_host}:{bastion_port} -o StrictHostKeyChecking=accept-new rmng@{clone_id}"
    )
}

/// Best-effort host (no scheme, port, or path) from a server base URL — used as the ssh
/// fallback when `ssh.publicHost` isn't configured. The CLI runs *inside* clones, so its
/// own server base is the control-server's internal docker address, not necessarily the
/// laptop-facing one; this is a best-effort guess, not a substitute for the real setting.
fn host_from_base(base: &str) -> &str {
    base.trim_start_matches("http://")
        .trim_start_matches("https://")
        .split(['/', ':'])
        .next()
        .unwrap_or(base)
}

/// `rmng ssh <host>`: print the ready-to-paste `ssh` one-liner that jumps through the
/// bastion into the clone. Fetches the redacted config for `ssh.publicHost` and
/// `listen.bastion`; falls back to a best-effort host guess (with a stderr note) when
/// `publicHost` isn't set, so the command on stdout stays copy-pasteable either way.
pub async fn ssh_cmd(client: &Client, host: &str) -> Result<u8> {
    let cfg = client.config().await?;
    let public_host = if !cfg.ssh.public_host.trim().is_empty() {
        cfg.ssh.public_host.clone()
    } else {
        let fallback = host_from_base(client.base()).to_string();
        eprintln!(
            "note: ssh.publicHost is not set; using {fallback} — set it in Settings → SSH Access for the correct laptop-facing address"
        );
        fallback
    };
    println!(
        "{}",
        build_ssh_command(&public_host, cfg.listen.bastion, host)
    );
    Ok(0)
}

/// What a desktop verb does with the daemon's `content` array once it comes back.
enum Kind {
    /// `monitors`/`windows`/`apps`: print the JSON text result, no screenshot.
    Query,
    /// `screenshot`: write the image and print its path.
    Screenshot,
    /// Everything else: print any text, then guarantee a post-action screenshot.
    Action,
}

/// The joined text of every `{type:"text"}` item in a daemon `content` array.
fn content_text(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            (item.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| item.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The base64 `data` of the first `{type:"image"}` item, if any.
fn content_image(content: &Value) -> Option<&str> {
    content.as_array().into_iter().flatten().find_map(|item| {
        (item.get("type").and_then(Value::as_str) == Some("image"))
            .then(|| item.get("data").and_then(Value::as_str))
            .flatten()
    })
}

/// Decode the image in `content`, write it to `out` (or the default
/// `$TMPDIR/rmng-<clone>-mon<N>.jpg`), and return its absolute path.
fn write_screenshot(
    content: &Value,
    clone: &str,
    monitor: Option<u32>,
    out: Option<&Path>,
) -> Result<PathBuf> {
    let data = content_image(content)
        .ok_or_else(|| anyhow!("daemon returned no image content for the screenshot"))?;
    let bytes = B64
        .decode(data)
        .map_err(|e| anyhow!("daemon image was not valid base64: {e}"))?;
    let path = out.map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir().join(format!("rmng-{clone}-mon{}.jpg", monitor.unwrap_or(0)))
    });
    std::fs::write(&path, &bytes)
        .map_err(|e| anyhow!("writing screenshot to {}: {e}", path.display()))?;
    Ok(std::fs::canonicalize(&path).unwrap_or(path))
}

/// Build a JSON args object from `(key, value)` pairs, dropping any null values so the
/// daemon only sees the keys the operator actually supplied.
fn args_obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in pairs {
        if !v.is_null() {
            m.insert(k.to_string(), v);
        }
    }
    Value::Object(m)
}

/// `rmng desktop <clone> <verb …>`. Maps the verb to a daemon tool, calls it, and
/// renders the result: query verbs print JSON, `screenshot` writes+prints a path, and
/// action verbs print any text then guarantee a post-action screenshot path.
pub async fn desktop(client: &Client, clone: &str, cmd: &DesktopCmd, json: bool) -> Result<u8> {
    let n = |v: Option<u32>| v.map(Value::from).unwrap_or(Value::Null);
    let i = |v: Option<i32>| v.map(Value::from).unwrap_or(Value::Null);
    let click_args = |x: &Option<i32>, y: &Option<i32>, monitor: &Option<u32>| {
        args_obj(vec![
            ("x", i(*x)),
            ("y", i(*y)),
            ("monitor", n(*monitor)),
        ])
    };

    // (tool, args, kind, monitor-for-screenshots, out path)
    let (tool, args, kind, monitor, out): (&str, Value, Kind, Option<u32>, Option<PathBuf>) =
        match cmd {
            DesktopCmd::Screenshot { monitor, out } => (
                "screenshot",
                args_obj(vec![("monitor", n(*monitor))]),
                Kind::Screenshot,
                *monitor,
                out.clone(),
            ),
            DesktopCmd::Monitors => ("list_monitors", args_obj(vec![]), Kind::Query, None, None),
            DesktopCmd::Windows => ("list_windows", args_obj(vec![]), Kind::Query, None, None),
            DesktopCmd::Apps => ("list_apps", args_obj(vec![]), Kind::Query, None, None),
            DesktopCmd::Move {
                x,
                y,
                monitor,
                out,
            } => (
                "mouse_move",
                args_obj(vec![("x", (*x).into()), ("y", (*y).into()), ("monitor", n(*monitor))]),
                Kind::Action,
                *monitor,
                out.clone(),
            ),
            DesktopCmd::Click { x, y, monitor, out } => (
                "left_click",
                click_args(x, y, monitor),
                Kind::Action,
                *monitor,
                out.clone(),
            ),
            DesktopCmd::Rclick { x, y, monitor, out } => (
                "right_click",
                click_args(x, y, monitor),
                Kind::Action,
                *monitor,
                out.clone(),
            ),
            DesktopCmd::Mclick { x, y, monitor, out } => (
                "middle_click",
                click_args(x, y, monitor),
                Kind::Action,
                *monitor,
                out.clone(),
            ),
            DesktopCmd::Dclick { x, y, monitor, out } => (
                "left_double_click",
                click_args(x, y, monitor),
                Kind::Action,
                *monitor,
                out.clone(),
            ),
            DesktopCmd::Scroll {
                amount,
                x,
                y,
                monitor,
                out,
            } => (
                "scroll",
                args_obj(vec![
                    ("amount", (*amount).into()),
                    ("x", i(*x)),
                    ("y", i(*y)),
                    ("monitor", n(*monitor)),
                ]),
                Kind::Action,
                *monitor,
                out.clone(),
            ),
            DesktopCmd::Key { keys, out } => (
                "key",
                args_obj(vec![("keys", keys.clone().into())]),
                Kind::Action,
                None,
                out.clone(),
            ),
            DesktopCmd::Type { text, out } => (
                "type",
                args_obj(vec![("text", text.clone().into())]),
                Kind::Action,
                None,
                out.clone(),
            ),
            DesktopCmd::Launch { id } => (
                "launch_app",
                args_obj(vec![("id", id.clone().into())]),
                Kind::Action,
                None,
                None,
            ),
            DesktopCmd::Movewin { id, monitor, mode } => (
                "move_window",
                args_obj(vec![
                    ("id", id.clone().into()),
                    ("monitor", n(*monitor)),
                    ("mode", mode.clone().map(Value::from).unwrap_or(Value::Null)),
                ]),
                Kind::Action,
                *monitor,
                None,
            ),
        };

    let content = client.desktop(clone, tool, args).await?;

    match kind {
        Kind::Query => {
            let text = content_text(&content);
            if json {
                // The daemon returns a JSON string inside a text item; re-emit it
                // parsed when it is valid JSON, else print it as-is.
                match serde_json::from_str::<Value>(&text) {
                    Ok(v) => emit_json(&v)?,
                    Err(_) => println!("{text}"),
                }
            } else {
                println!("{text}");
            }
            Ok(0)
        }
        Kind::Screenshot => {
            let path = write_screenshot(&content, clone, monitor, out.as_deref())?;
            println!("{}", path.display());
            Ok(0)
        }
        Kind::Action => {
            let text = content_text(&content);
            if !text.is_empty() {
                println!("{text}");
            }
            // Guarantee a settle screenshot: reuse the action's own image if it has
            // one, else make a follow-up `screenshot` call.
            let shot = if content_image(&content).is_some() {
                content
            } else {
                client
                    .desktop(clone, "screenshot", args_obj(vec![("monitor", n(monitor))]))
                    .await?
            };
            let path = write_screenshot(&shot, clone, monitor, out.as_deref())?;
            println!("{}", path.display());
            Ok(0)
        }
    }
}

/// `rmng exec <clone> [flags] -- <cmd…>`. Reads piped stdin (base64), runs the command
/// in the clone, splits stdout/stderr to our own streams (or one JSON object with
/// `--json`), and exits with the command's own exit code.
#[allow(clippy::too_many_arguments)]
pub async fn exec(
    client: &Client,
    clone: &str,
    user: Option<&str>,
    workdir: Option<&str>,
    env: &[String],
    cmd: &[String],
    json: bool,
) -> Result<u8> {
    use std::io::{IsTerminal, Read, Write};

    // Pass through piped stdin so `echo hi | rmng exec c -- cat` works; a TTY stdin is
    // left untouched (this command is non-interactive).
    let stdin_b64 = if std::io::stdin().is_terminal() {
        None
    } else {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        (!buf.is_empty()).then(|| B64.encode(&buf))
    };

    let req = wire::ExecRequest {
        cmd: cmd.to_vec(),
        user: user.map(str::to_string),
        workdir: workdir.map(str::to_string),
        env: env.to_vec(),
        stdin_b64,
    };
    let result = client.exec(clone, &req).await?;

    if json {
        emit_json(&result)?;
    } else {
        print!("{}", result.stdout);
        std::io::stdout().flush().ok();
        eprint!("{}", result.stderr);
        std::io::stderr().flush().ok();
    }
    // Surface the command's own status. A value outside 0..=255 means docker gave no
    // exit code (server sentinel -1) — report 125 (docker's own "exec failure" code)
    // rather than masking an unknown outcome as success.
    Ok(match result.exit_code {
        c @ 0..=255 => c as u8,
        _ => 125,
    })
}

/// Used by `main` for a friendlier connection-refused hint.
pub fn connect_hint(base: &str, err: &anyhow::Error) -> String {
    format!("{err:#}\n(server: {base} — set --server or $RMNG_CONTROL_URL)")
}

#[allow(dead_code)]
fn _assert_state_is_wire(st: ControlState) -> ControlState {
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_command_is_the_inline_jump_one_liner() {
        assert_eq!(
            build_ssh_command("rmng.example.com", 2222, "w-cp-claude"),
            "ssh -J rmng@rmng.example.com:2222 -o StrictHostKeyChecking=accept-new rmng@w-cp-claude"
        );
    }

    #[test]
    fn host_from_base_strips_scheme_port_and_path() {
        assert_eq!(host_from_base("http://rmng-control:9000"), "rmng-control");
        assert_eq!(
            host_from_base("https://rmng.example.com/"),
            "rmng.example.com"
        );
        assert_eq!(host_from_base("localhost:9000"), "localhost");
    }
}
