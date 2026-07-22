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

use crate::args::{AccountCmd, DesktopCmd, ImageCmd, RescaleArgs, WaitArgs};
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
                h.local_ip.clone().unwrap_or_default(),
                h.source.clone().unwrap_or_default(),
                h.preset_name.clone().unwrap_or_default(),
                h.group.clone().unwrap_or_default(),
            ]
        })
        .collect();
    print!(
        "{}",
        table(&["ID", "IP", "IMAGE", "PRESET", "GROUP"], &rows)
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
        AccountCmd::Ls {
            claude,
            codex,
            gemini,
        } => {
            let st = client.state().await?;
            let accounts: Vec<_> = st
                .usage_groups
                .iter()
                .flat_map(|g| g.accounts.iter())
                .filter(|a| {
                    if *claude {
                        // A missing provider defaults to Claude (legacy rows).
                        matches!(a.provider, Some(Provider::Claude) | None)
                    } else if *codex {
                        matches!(a.provider, Some(Provider::Codex))
                    } else if *gemini {
                        matches!(a.provider, Some(Provider::Antigravity))
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
                        match a.provider {
                            Some(Provider::Codex) => "codex".into(),
                            Some(Provider::Antigravity) => "gemini".into(),
                            _ => "claude".into(),
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
                        pct(&a.fable),
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
                        "FABLE",
                        "ERROR"
                    ],
                    &rows
                )
            );
            // Account groups come from config (redacted view), not state. Membership lives in
            // each group's CLIProxyAPI instance auth-dir and is surfaced above via usage_groups.
            if let Ok(cfg) = client.config().await
                && !cfg.groups.is_empty()
            {
                let names = cfg
                    .groups
                    .iter()
                    .map(|g| g.name.clone())
                    .collect::<Vec<_>>()
                    .join("  ");
                println!("groups: {names}");
            }
            Ok(0)
        }
        // `account` is now the GROUP name to bind this clone to ("" / "none" clears it).
        // Groups are provider-agnostic under the group-proxy model, so `--codex` is ignored.
        AccountCmd::Swap {
            host,
            account,
            codex: _,
        } => {
            let group = match account.as_str() {
                "" | "none" => None,
                g => Some(g),
            };
            let reply = client.set_host_group(host, group).await?;
            if json {
                emit_json(&reply)?;
            } else {
                println!("set {host} group → {}", group.unwrap_or("none"));
            }
            Ok(0)
        }
        // Account membership is per-group now: remove this email's credential from every group
        // it appears in (the auth file name follows the `<provider>-<email>.json` convention).
        AccountCmd::Rm { account, codex } => {
            let file = format!("{}-{account}.json", if *codex { "codex" } else { "claude" });
            let st = client.state().await?;
            let mut removed: Vec<String> = Vec::new();
            for g in &st.usage_groups {
                if g.accounts.iter().any(|a| &a.email == account) {
                    client.delete_group_account(&g.name, &file).await?;
                    removed.push(g.name.clone());
                }
            }
            if json {
                emit_json(
                    &serde_json::json!({ "ok": true, "account": account, "removedFrom": removed }),
                )?;
            } else if removed.is_empty() {
                println!("no group currently lists {account}");
            } else {
                println!(
                    "removed {account} from {} group(s): {}",
                    removed.len(),
                    removed.join(", ")
                );
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

fn validate_ssh_host(st: &ControlState, host: &str) -> Result<()> {
    if st.hosts.iter().any(|h| h.id == host) {
        Ok(())
    } else {
        bail!("unknown host '{host}' (see `rmng ps`)")
    }
}

/// `rmng ssh <host>`: print the ready-to-paste `ssh` one-liner that jumps through the
/// bastion into the clone. Fetches the redacted config for `ssh.publicHost` and
/// `listen.bastion`; falls back to a best-effort host guess (with a stderr note) when
/// `publicHost` isn't set, so the command on stdout stays copy-pasteable either way.
pub async fn ssh_cmd(client: &Client, host: &str) -> Result<u8> {
    let st = client.state().await?;
    validate_ssh_host(&st, host)?;
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

/// Decode a JPEG, resize to `(w, h)` pixels with bilinear filtering, and
/// re-encode as JPEG (quality 85 — good enough for screen content, much
/// smaller than the daemon's 95-quality default). `None` for `target` is a
/// pass-through. Returns an error if the bytes aren't a valid JPEG (the
/// daemon always sends JPEG; if we ever switch to PNG/WebP this is the
/// place to extend the format list).
fn rescale_jpeg(bytes: &[u8], target: Option<(u32, u32)>) -> Result<Vec<u8>> {
    let Some((w, h)) = target else {
        return Ok(bytes.to_vec());
    };
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
        .map_err(|e| anyhow!("--rescale-screen: decoding source JPEG: {e}"))?;
    let resized = img.resize_exact(w, h, image::imageops::FilterType::Triangle);
    let mut out = Vec::with_capacity(bytes.len() / 4);
    let mut cursor = std::io::Cursor::new(&mut out);
    {
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 85);
        encoder
            .encode(
                resized.as_bytes(),
                resized.width(),
                resized.height(),
                resized.color().into(),
            )
            .map_err(|e| anyhow!("--rescale-screen: encoding resized JPEG: {e}"))?;
    }
    Ok(out)
}

/// Decode the image in `content`, write it to `out` (or the default
/// `$TMPDIR/rmng-<clone>-mon<N>.jpg`), and return its absolute path. When
/// `rescale` is `Some((w, h))` the JPEG is decoded, resized, and re-encoded
/// before being written — used by `--rescale-screen` to shrink the file
/// for a downstream vision model (or upscale for clarity).
fn write_screenshot(
    content: &Value,
    clone: &str,
    monitor: Option<u32>,
    out: Option<&Path>,
    rescale: Option<(u32, u32)>,
) -> Result<PathBuf> {
    let data = content_image(content)
        .ok_or_else(|| anyhow!("daemon returned no image content for the screenshot"))?;
    let bytes = B64
        .decode(data)
        .map_err(|e| anyhow!("daemon image was not valid base64: {e}"))?;
    let bytes = rescale_jpeg(&bytes, rescale)?;
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

/// Rescale a single axis from a normalized source space to a target pixel
/// range, rounding to nearest. `src_max` is the upper bound of the source
/// space (e.g. 1000 for MiniMax M3); `dst_max` is the destination size in
/// pixels. Returns a plain `i32` — callers that want None semantics use
/// [`rescale_optional`].
fn rescale_axis(v: i32, src_min: i32, src_max: i32, dst_max: i32) -> i32 {
    let span = (src_max - src_min) as i64;
    let offset = (v - src_min) as i64;
    // round-to-nearest: (offset * dst_max + span/2) / span
    let scaled = (offset * dst_max as i64 + span / 2) / span;
    src_min + scaled as i32
}

/// Walk the daemon's `list_monitors` and return the W×H of the monitor the
/// caller is targeting (the one they passed via `--monitor`, or the first one
/// if they didn't). Used to convert normalized input coords (e.g. M3's
/// 0–1000) into pixel space. Bails with a useful message when the daemon
/// reports no monitors (clone not ready, MCP wedged, etc.).
async fn monitor_size(client: &Client, clone: &str, monitor: Option<u32>) -> Result<(i32, i32)> {
    let content = client
        .desktop(clone, "list_monitors", Value::Object(Default::default()))
        .await?;
    let text = content
        .get(0)
        .and_then(|c| c.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("list_monitors: no text in response"))?;
    let mons: Vec<Value> =
        serde_json::from_str(text).map_err(|e| anyhow!("list_monitors: not JSON: {e}: {text}"))?;
    if mons.is_empty() {
        bail!("list_monitors: clone has no monitors");
    }
    let pick = monitor
        .and_then(|id| {
            mons.iter()
                .find(|m| m.get("id").and_then(Value::as_u64) == Some(id as u64))
        })
        .unwrap_or_else(|| &mons[0]);
    let w = pick
        .get("width")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("list_monitors: monitor missing width"))? as i32;
    let h = pick
        .get("height")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("list_monitors: monitor missing height"))? as i32;
    Ok((w, h))
}

/// Rescale `(x, y)` from `src` (a `(min, max)` tuple, or `None` for
/// pass-through) to pixel coords on the targeted monitor. If `screen` is
/// `Some((w, h))` use that directly (the `--rescale-screen` path); otherwise
/// ask the daemon for the target monitor's W×H. Required for `mouse_move`
/// (which always takes x/y) — for verbs where the coords are optional, use
/// [`rescale_optional`] instead.
async fn rescale_coords(
    client: &Client,
    clone: &str,
    src: Option<(i32, i32)>,
    screen: Option<(i32, i32)>,
    monitor: Option<u32>,
    x: i32,
    y: i32,
) -> Result<(i32, i32)> {
    let Some((lo, hi)) = src else {
        return Ok((x, y));
    };
    let (w, h) = match screen {
        Some(s) => s,
        None => monitor_size(client, clone, monitor).await?,
    };
    Ok((rescale_axis(x, lo, hi, w), rescale_axis(y, lo, hi, h)))
}

/// Same as [`rescale_coords`] but accepts `Option<i32>` for click verbs whose
/// `x`/`y` are optional (a bare `rmng desktop c click` re-clicks the current
/// pointer position). `None` is passed through untouched.
async fn rescale_optional(
    client: &Client,
    clone: &str,
    src: Option<(i32, i32)>,
    screen: Option<(i32, i32)>,
    monitor: Option<u32>,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(Option<i32>, Option<i32>)> {
    let Some((lo, hi)) = src else {
        return Ok((x, y));
    };
    let (w, h) = match screen {
        Some(s) => s,
        None => monitor_size(client, clone, monitor).await?,
    };
    Ok((
        x.map(|v| rescale_axis(v, lo, hi, w)),
        y.map(|v| rescale_axis(v, lo, hi, h)),
    ))
}

/// `rmng desktop <clone> <verb …>`. Maps the verb to a daemon tool, calls it, and
/// renders the result: query verbs print JSON, `screenshot` writes+prints a path, and
/// action verbs print any text then guarantee a post-action screenshot path.
pub async fn desktop(client: &Client, clone: &str, cmd: &DesktopCmd, json: bool) -> Result<u8> {
    let n = |v: Option<u32>| v.map(Value::from).unwrap_or(Value::Null);
    let i = |v: Option<i32>| v.map(Value::from).unwrap_or(Value::Null);

    // Parse the verb's `--rescale-cursor` value (or None when unset). We capture
    // it as a closure so each arm of the match can apply it without re-writing
    // the `?` plumbing. `rescale` (the field on each verb) and `parse_rescale_cursor`
    // (this closure) share a name intentionally: shadowing the field with the
    // closure would also work, but keeping them distinct makes the type
    // signature explicit at every call site.
    let parse_rescale_cursor = |r: &RescaleArgs| r.parsed_cursor().map_err(anyhow::Error::msg);
    // Same trick for `--rescale-screen` — the user's override of monitor size
    // (skips the `list_monitors` auto-detect round-trip).
    let parse_rescale_screen = |r: &RescaleArgs| r.parsed_screen().map_err(anyhow::Error::msg);
    // `--rescale-screen` is independent of the verb, so we resolve it once
    // up front and use the same value for every daemon call's response —
    // both the explicit `screenshot` verb and the auto-snap after an action.
    // We sniff it from the verb's own `rescale` field; a no-op for verbs
    // that don't carry it (Monitors/Windows/Apps/Key/Type/Launch/Movewin)
    // because their match arms synthesize a default `RescaleArgs` below.
    let screen_target: Option<(u32, u32)> = match cmd {
        DesktopCmd::Screenshot { rescale, .. }
        | DesktopCmd::Move { rescale, .. }
        | DesktopCmd::Click { rescale, .. }
        | DesktopCmd::Rclick { rescale, .. }
        | DesktopCmd::Mclick { rescale, .. }
        | DesktopCmd::Dclick { rescale, .. }
        | DesktopCmd::Scroll { rescale, .. } => match parse_rescale_screen(rescale) {
            Ok(t) => t.map(|(w, h)| (w as u32, h as u32)),
            Err(e) => return Err(anyhow::Error::msg(e)),
        },
        _ => None,
    };

    // (tool, args, kind, monitor-for-screenshots, out path)
    let (tool, args, kind, monitor, out): (&str, Value, Kind, Option<u32>, Option<PathBuf>) =
        match cmd {
            DesktopCmd::Screenshot { monitor, out, .. } => (
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
                rescale,
            } => {
                let (x, y) = rescale_coords(
                    client,
                    clone,
                    parse_rescale_cursor(rescale)?,
                    parse_rescale_screen(rescale)?,
                    *monitor,
                    *x,
                    *y,
                )
                .await?;
                (
                    "mouse_move",
                    args_obj(vec![
                        ("x", x.into()),
                        ("y", y.into()),
                        ("monitor", n(*monitor)),
                    ]),
                    Kind::Action,
                    *monitor,
                    out.clone(),
                )
            }
            DesktopCmd::Click {
                x,
                y,
                monitor,
                out,
                rescale,
            } => {
                let (x, y) = rescale_optional(
                    client,
                    clone,
                    parse_rescale_cursor(rescale)?,
                    parse_rescale_screen(rescale)?,
                    *monitor,
                    *x,
                    *y,
                )
                .await?;
                (
                    "left_click",
                    args_obj(vec![("x", i(x)), ("y", i(y)), ("monitor", n(*monitor))]),
                    Kind::Action,
                    *monitor,
                    out.clone(),
                )
            }
            DesktopCmd::Rclick {
                x,
                y,
                monitor,
                out,
                rescale,
            } => {
                let (x, y) = rescale_optional(
                    client,
                    clone,
                    parse_rescale_cursor(rescale)?,
                    parse_rescale_screen(rescale)?,
                    *monitor,
                    *x,
                    *y,
                )
                .await?;
                (
                    "right_click",
                    args_obj(vec![("x", i(x)), ("y", i(y)), ("monitor", n(*monitor))]),
                    Kind::Action,
                    *monitor,
                    out.clone(),
                )
            }
            DesktopCmd::Mclick {
                x,
                y,
                monitor,
                out,
                rescale,
            } => {
                let (x, y) = rescale_optional(
                    client,
                    clone,
                    parse_rescale_cursor(rescale)?,
                    parse_rescale_screen(rescale)?,
                    *monitor,
                    *x,
                    *y,
                )
                .await?;
                (
                    "middle_click",
                    args_obj(vec![("x", i(x)), ("y", i(y)), ("monitor", n(*monitor))]),
                    Kind::Action,
                    *monitor,
                    out.clone(),
                )
            }
            DesktopCmd::Dclick {
                x,
                y,
                monitor,
                out,
                rescale,
            } => {
                let (x, y) = rescale_optional(
                    client,
                    clone,
                    parse_rescale_cursor(rescale)?,
                    parse_rescale_screen(rescale)?,
                    *monitor,
                    *x,
                    *y,
                )
                .await?;
                (
                    "left_double_click",
                    args_obj(vec![("x", i(x)), ("y", i(y)), ("monitor", n(*monitor))]),
                    Kind::Action,
                    *monitor,
                    out.clone(),
                )
            }
            DesktopCmd::Scroll {
                amount,
                x,
                y,
                monitor,
                out,
                rescale,
            } => {
                let (x, y) = rescale_optional(
                    client,
                    clone,
                    parse_rescale_cursor(rescale)?,
                    parse_rescale_screen(rescale)?,
                    *monitor,
                    *x,
                    *y,
                )
                .await?;
                (
                    "scroll",
                    args_obj(vec![
                        ("amount", (*amount).into()),
                        ("x", i(x)),
                        ("y", i(y)),
                        ("monitor", n(*monitor)),
                    ]),
                    Kind::Action,
                    *monitor,
                    out.clone(),
                )
            }
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
            let path = write_screenshot(&content, clone, monitor, out.as_deref(), screen_target)?;
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
            let path = write_screenshot(&shot, clone, monitor, out.as_deref(), screen_target)?;
            println!("{}", path.display());
            Ok(0)
        }
    }
}

/// How long `rmng exec` waits for piped stdin to present its first byte before deciding
/// there is nothing to forward. Real pipes (`echo hi | rmng exec …`) are ready at once, so
/// this only bounds the wait for an *idle* open pipe (a harness / driver / script that
/// holds stdin open without writing), which must never hang the command.
const STDIN_POLL_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

/// Whether stdin has input ready to read — data buffered, or the write end closed (EOF) —
/// waiting up to `grace`. Returns `false` only for an idle open pipe (nothing ready within
/// `grace`), so [`exec`] can skip the otherwise-unbounded `read_to_end` that would hang the
/// command. A regular-file/`/dev/null` redirect and a live pipe both report ready promptly;
/// once ready the caller drains the whole stream, so large piped input is unaffected.
/// Unix readiness comes from `poll(2)`; other platforms conservatively report ready
/// (the historical always-read behavior).
#[cfg(unix)]
fn stdin_has_input(grace: std::time::Duration) -> bool {
    use std::os::unix::io::AsRawFd;
    let mut pfd = libc::pollfd {
        fd: std::io::stdin().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = grace.as_millis().min(i32::MAX as u128) as i32;
    // >0: readable or hung-up (EOF) → drain it. 0: timed out (idle pipe) → forward no stdin.
    // <0: poll error → fall back to attempting the read (the old behavior).
    unsafe { libc::poll(&mut pfd, 1, ms) != 0 }
}

#[cfg(not(unix))]
fn stdin_has_input(_grace: std::time::Duration) -> bool {
    true
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
    // left untouched (this command is non-interactive). Crucially we must NOT blindly
    // `read_to_end` every non-terminal stdin: when `rmng exec` is launched from a
    // script, an agent/tool harness, or a fleet driver, stdin is typically an *open*
    // pipe with nothing to send, and a blocking read there hangs the command forever
    // before it ever runs (the historical `rmng exec` hang). Gate the drain on a
    // readiness poll — only read stdin once it actually has data (or has hit EOF); an
    // idle open pipe yields nothing within the grace window and we forward no stdin. A
    // ready fd still drains fully, so large piped input is fine (the poll only bounds
    // the wait for the first byte).
    let stdin_b64 = if std::io::stdin().is_terminal() || !stdin_has_input(STDIN_POLL_GRACE) {
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
    fn validate_ssh_host_rejects_unknown_host() {
        let st = ControlState {
            hosts: vec![wire::Host {
                id: "pega-herms".into(),
                managed: true,
                ..Default::default()
            }],
            ..Default::default()
        };

        let err = validate_ssh_host(&st, "herms").expect_err("host suffix must not match");
        assert_eq!(err.to_string(), "unknown host 'herms' (see `rmng ps`)");
        validate_ssh_host(&st, "pega-herms").expect("exact host id should match");
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

    #[test]
    fn rescale_axis_0_1000_to_2560x1440_endpoints() {
        // 0 → 0 and 1000 → dst_max, by definition.
        assert_eq!(rescale_axis(0, 0, 1000, 2560), 0);
        assert_eq!(rescale_axis(1000, 0, 1000, 2560), 2560);
        assert_eq!(rescale_axis(0, 0, 1000, 1440), 0);
        assert_eq!(rescale_axis(1000, 0, 1000, 1440), 1440);
    }

    #[test]
    fn rescale_axis_0_1000_to_1920x1080_midpoint_is_pixels() {
        // 500/1000 * 1920 = 960, 500/1000 * 1080 = 540 — the whole point of
        // the flag is that the caller's "center" lands on the actual center.
        assert_eq!(rescale_axis(500, 0, 1000, 1920), 960);
        assert_eq!(rescale_axis(500, 0, 1000, 1080), 540);
    }

    #[test]
    fn rescale_axis_quarter_and_three_quarters() {
        // 250/1000 * 1920 = 480, 750/1000 * 1920 = 1440
        assert_eq!(rescale_axis(250, 0, 1000, 1920), 480);
        assert_eq!(rescale_axis(750, 0, 1000, 1920), 1440);
    }

    #[test]
    fn rescale_axis_arbitrary_min_max_range() {
        // 0..=99 (UIKit points) to 0..=2560 (Retina pixels) at 128 points:
        // 128/99 * 2560 ≈ 3309.9, with round-to-nearest that's 3310.
        assert_eq!(rescale_axis(128, 0, 99, 2560), 3310);
    }

    #[test]
    fn rescale_args_parsed_cursor_accepts_0_1000() {
        let r = RescaleArgs {
            rescale_cursor: Some("0-1000".into()),
            rescale_screen: None,
        };
        assert_eq!(r.parsed_cursor().unwrap(), Some((0, 1000)));
    }

    #[test]
    fn rescale_args_parsed_cursor_bare_max_assumes_zero_origin() {
        let r = RescaleArgs {
            rescale_cursor: Some("1000".into()),
            rescale_screen: None,
        };
        assert_eq!(r.parsed_cursor().unwrap(), Some((0, 1000)));
    }

    #[test]
    fn rescale_args_parsed_cursor_unset_is_none() {
        let r = RescaleArgs::default();
        assert!(r.parsed_cursor().unwrap().is_none());
    }

    #[test]
    fn rescale_args_parsed_cursor_rejects_inverted_range() {
        let r = RescaleArgs {
            rescale_cursor: Some("1000-0".into()),
            rescale_screen: None,
        };
        assert!(r.parsed_cursor().is_err());
    }

    #[test]
    fn rescale_args_parsed_cursor_rejects_zero_max() {
        let r = RescaleArgs {
            rescale_cursor: Some("0".into()),
            rescale_screen: None,
        };
        assert!(r.parsed_cursor().is_err());
    }

    #[test]
    fn rescale_args_parsed_cursor_rejects_garbage() {
        let r = RescaleArgs {
            rescale_cursor: Some("not-a-number".into()),
            rescale_screen: None,
        };
        assert!(r.parsed_cursor().is_err());
    }

    #[test]
    fn rescale_args_parsed_cursor_screen_accepts_lowercase_x() {
        let r = RescaleArgs {
            rescale_cursor: Some("0-1000".into()),
            rescale_screen: Some("1920x1080".into()),
        };
        assert_eq!(r.parsed_screen().unwrap(), Some((1920, 1080)));
    }

    #[test]
    fn rescale_args_parsed_cursor_screen_accepts_uppercase_x() {
        let r = RescaleArgs {
            rescale_cursor: Some("0-1000".into()),
            rescale_screen: Some("2560X1440".into()),
        };
        assert_eq!(r.parsed_screen().unwrap(), Some((2560, 1440)));
    }

    #[test]
    fn rescale_args_parsed_cursor_screen_unset_is_none() {
        let r = RescaleArgs {
            rescale_cursor: Some("0-1000".into()),
            rescale_screen: None,
        };
        assert!(r.parsed_screen().unwrap().is_none());
    }

    #[test]
    fn rescale_args_parsed_cursor_screen_rejects_missing_separator() {
        let r = RescaleArgs {
            rescale_cursor: Some("0-1000".into()),
            rescale_screen: Some("1920-1080".into()),
        };
        assert!(r.parsed_screen().is_err());
    }

    #[test]
    fn rescale_args_parsed_screen_rejects_zero() {
        let r = RescaleArgs {
            rescale_cursor: Some("0-1000".into()),
            rescale_screen: Some("0x1080".into()),
        };
        assert!(r.parsed_screen().is_err());
    }

    #[test]
    fn rescale_jpeg_passthrough_when_target_is_none() {
        // `target = None` should be a copy; the bytes come out untouched.
        // We can't easily produce a "real" JPEG here without pulling in an
        // encoder, so just feed an opaque payload and confirm the function
        // returns it as-is. (The format check only happens when a target is
        // set; that's the point of this test.)
        let payload = b"\xff\xd8\xff\xe0 not a real jpeg but the function won't see it";
        let out = rescale_jpeg(payload, None).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn rescale_jpeg_resizes_a_known_image() {
        // Build a 4×2 solid-red RGB image, encode as JPEG, then rescale to
        // 2×1 and confirm the output is a valid JPEG with the requested
        // dimensions. This exercises the full decode→resize→encode path
        // without needing any external test fixture.
        use image::{ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(4, 2, |_, _| Rgb([255u8, 0, 0]));
        let mut src_jpeg = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new(&mut src_jpeg);
        encoder
            .encode(
                img.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        let src_len = src_jpeg.len();
        assert!(src_len > 0);

        // None → no-op
        let passthrough = rescale_jpeg(&src_jpeg, None).unwrap();
        assert_eq!(passthrough, src_jpeg);

        // Resize to 2×1, then decode the result and check dimensions.
        let resized = rescale_jpeg(&src_jpeg, Some((2, 1))).unwrap();
        assert!(!resized.is_empty());
        let decoded = image::load_from_memory_with_format(&resized, image::ImageFormat::Jpeg)
            .expect("rescaled output is a valid JPEG");
        assert_eq!(decoded.width(), 2);
        assert_eq!(decoded.height(), 1);
    }
}
