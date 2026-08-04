//! One handler per subcommand: a thin client call + rendering. Handlers return the
//! process exit code (0 ok, 3 operation failed, 4 timeout); transport/API errors
//! bubble up as `anyhow` errors and exit 1 from `main`.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use control_client::{Client, CloneOpts};
use serde_json::{Value, json};
use wire::{ContainerStats, ControlState, MonitorState, Operation, Provider};

use crate::args::{
    AccountCmd, CreateArgs, DesktopCmd, ImageCmd, Provider as CliProvider, WaitArgs,
};
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

fn cpu_pct(cpu_pct: f64) -> String {
    if cpu_pct < 1.0 {
        format!("{cpu_pct:.1}%")
    } else {
        format!("{cpu_pct:.0}%")
    }
}

fn ram(stats: &ContainerStats) -> String {
    if stats.mem_limit == 0 {
        human_size(stats.mem_used)
    } else {
        format!(
            "{}/{}",
            human_size(stats.mem_used),
            human_size(stats.mem_limit)
        )
    }
}

fn clone_status(archived: bool, monitor_state: Option<MonitorState>) -> String {
    if archived {
        return "archived".to_string();
    }
    match monitor_state {
        Some(MonitorState::Working) => "working".to_string(),
        Some(MonitorState::Idle) => "idle".to_string(),
        Some(MonitorState::Offline) => "offline".to_string(),
        None => String::new(),
    }
}

pub async fn clone_ls(client: &Client, json: bool) -> Result<u8> {
    let (st, stats) = tokio::try_join!(client.state(), client.stats())?;
    // `--json` emits the JOINED view the human table shows — each clone object with its live
    // `stats` nested — so an agent parsing JSON gets CPU/RAM too (the raw wire `ControlState`
    // omits those volatile metrics). Stable CLI-owned shape; see docs/CLI.md.
    if json {
        let clones: Vec<Value> = st
            .hosts
            .iter()
            .map(|h| {
                let mut o = serde_json::to_value(h).unwrap_or_else(|_| serde_json::json!({}));
                o["stats"] = serde_json::to_value(stats.get(&h.id)).unwrap_or(Value::Null);
                // A derived per-provider view of the six flat account fields. They ARE all
                // present on the object already (serde), but answering "what account is this
                // clone on?" from them takes three-way logic per provider — `email` is null
                // while a selection is still `auto` and unresolved, and `pool` is null unless
                // the selection names one. Collapsing that here means a consumer reads one
                // place instead of reimplementing the precedence, and `selection` stays
                // available for the distinction that actually matters: `auto` (may be swapped
                // under you) vs a pinned email vs `none` (deliberately tokenless).
                o["accounts"] = serde_json::json!({
                    "claude": {
                        "selection": h.claude_selection,
                        "email": h.claude_account_email,
                        "pool": h.claude_group,
                    },
                    "codex": {
                        "selection": h.codex_selection,
                        "email": h.codex_account_email,
                        "pool": h.codex_group,
                    },
                });
                o
            })
            .collect();
        emit_json(&serde_json::json!({
            "selected": st.selected,
            "clones": clones,
            "operations": st.operations,
        }))?;
        return Ok(0);
    }

    // Order rows as a one-level tree: each top-level clone followed by its sub clones, which are
    // indented under it. A child whose parent isn't present renders at top level so nothing is
    // hidden. Order within each level is the server's clone Vec order.
    let present: std::collections::HashSet<&str> = st.hosts.iter().map(|h| h.id.as_str()).collect();
    let mut ordered: Vec<(&_, bool)> = Vec::with_capacity(st.hosts.len());
    for h in &st.hosts {
        let is_top = h.parent.as_deref().is_none_or(|p| !present.contains(p));
        if !is_top {
            continue;
        }
        ordered.push((h, false));
        for c in &st.hosts {
            if c.parent.as_deref() == Some(h.id.as_str()) {
                ordered.push((c, true));
            }
        }
    }
    let rows: Vec<Vec<String>> = ordered
        .iter()
        .map(|(h, is_child)| {
            let sel = if st.selected.as_deref() == Some(&h.id) {
                "*"
            } else {
                ""
            };
            let id_cell = if *is_child {
                format!("└─ {}{}", h.id, sel)
            } else {
                format!("{}{}", h.id, sel)
            };
            let stats = stats.get(&h.id);
            vec![
                id_cell,
                h.local_ip.clone().unwrap_or_default(),
                h.source.clone().unwrap_or_default(),
                h.preset_name.clone().unwrap_or_default(),
                h.claude_account_email
                    .clone()
                    .or_else(|| h.claude_selection.clone())
                    .unwrap_or_default(),
                h.codex_account_email
                    .clone()
                    .or_else(|| h.codex_selection.clone())
                    .unwrap_or_default(),
                stats
                    .map(|stats| cpu_pct(stats.cpu_pct))
                    .unwrap_or_default(),
                stats.map(ram).unwrap_or_default(),
                clone_status(h.archived, h.monitor_state),
            ]
        })
        .collect();
    print!(
        "{}",
        table(
            &[
                "ID", "IP", "IMAGE", "PRESET", "CLAUDE", "CODEX", "CPU", "RAM", "STATUS",
            ],
            &rows,
        )
    );
    Ok(0)
}

pub async fn select(client: &Client, clone: Option<&str>, none: bool, json: bool) -> Result<u8> {
    if clone.is_none() && !none {
        bail!("provide a clone id, or --none to clear the selection");
    }
    let target = if none { None } else { clone };
    if let Some(id) = target {
        let st = client.state().await?;
        if !st.hosts.iter().any(|h| h.id == id) {
            bail!("unknown clone '{id}' (see `rmng clone ls`)");
        }
    }
    let st = client.activate(target).await?;
    if json {
        emit_json(&serde_json::json!({ "selected": st.selected }))?;
    } else {
        match target {
            Some(id) => println!("selected {id}"),
            None => println!("selection cleared"),
        }
    }
    Ok(0)
}

/// Build the shared [`CloneOpts`] from the flags every create verb carries. `preset` is
/// passed separately because only two of the four verbs expose one — the ticket verbs
/// auto-select it server-side, exactly as the web dialog does.
fn clone_opts<'a>(
    common: &'a CreateArgs,
    preset: Option<&'a str>,
    agent_instructions: Option<&'a String>,
    claude_instructions: Option<&'a String>,
) -> CloneOpts<'a> {
    CloneOpts {
        claude_account: common.claude_account.as_deref(),
        codex_account: common.codex_account.as_deref(),
        preset,
        headless: common.headless,
        parent: common.parent.as_deref(),
        top_level: common.top_level,
        agent_instructions: agent_instructions.map(String::as_str),
        claude_instructions: claude_instructions.map(String::as_str),
    }
}

/// `rmng clone create <hostname> --from <image>` — exact-hostname clone, no ticket.
pub async fn clone_create(
    client: &Client,
    hostname: &str,
    preset: Option<&str>,
    no_preset: bool,
    common: &CreateArgs,
    json: bool,
) -> Result<u8> {
    // `--no-preset` maps to the `none` sentinel the server treats as "no preset" (and, for a
    // sub clone, opt out of inheriting the parent's). Omitted ⇒ inherit. The account flags need
    // no such pair: `--claude-account none` already expresses "no account", and omitting them
    // falls through the request → parent → preset-default chain.
    let preset = if no_preset { Some("none") } else { preset };
    let op = client
        .clone_create(
            &common.from,
            json!({ "hostname": hostname }),
            &clone_opts(common, preset, None, None),
        )
        .await?;
    started(client, op, &common.wait, json, "clone").await
}

/// The resolved-metadata `linear` mode of `POST /api/clone`: an issue this CLI already
/// looked up (or opened) in Linear, said the way the server reads it. The server makes no
/// Linear call of its own for this body; it derives the hostname and starts the clone.
fn linear_mode(issue: &crate::linear::IssueInfo) -> Value {
    json!({ "linear": {
        "workspace": issue.prefix,
        "ticket": issue.identifier,
        "ticketUrl": issue.url,
        "branch": issue.branch,
        "title": issue.title,
        // A clone stores one label, the issue's first. Blank when it has none, which the
        // server reads as no label.
        "label": issue.labels.first().map(String::as_str).unwrap_or(""),
    }})
}

/// `rmng clone create-from-ticket <link-or-id>` — clone for an existing Linear ticket. No `--preset`:
/// the server auto-selects it from the ticket's team prefix, matching the web dialog.
///
/// The ticket lookup happens here, against `api.linear.app`, with the preset Linear keys
/// `GET /api/config` vends. Whichever key sees the issue also moves it to In Progress.
pub async fn clone_create_from_ticket(
    client: &Client,
    ticket: &str,
    agent_instructions: Option<&String>,
    claude_instructions: Option<&String>,
    common: &CreateArgs,
    json: bool,
) -> Result<u8> {
    let http = reqwest::Client::new();
    let r = crate::linear::parse_ticket_ref(ticket)?;
    let cfg = client.config().await?;
    let keys: Vec<&str> = cfg.presets.iter().map(|p| p.linear_key.as_str()).collect();
    let (issue, key) = crate::linear::fetch_issue_any(&http, &keys, &r).await?;
    // Best effort: a ticket that refuses to move is not a reason to withhold the clone.
    if let Err(e) = crate::linear::ensure_in_progress(&http, &key, &issue).await {
        eprintln!("warning: could not move {} to In Progress: {e}", issue.identifier);
    }
    let op = client
        .clone_create(
            &common.from,
            linear_mode(&issue),
            &clone_opts(common, None, agent_instructions, claude_instructions),
        )
        .await?;
    started(client, op, &common.wait, json, "clone").await
}

/// `rmng clone create-with-new-ticket --team <key> --title <t>` — create the Linear ticket, then clone
/// for it. The team key picks the preset (whose API key opens the issue), so again no
/// `--preset`. `description` is markdown, taken verbatim: unlike the web dialog this verb
/// has no image upload behind it, so there is nothing to re-host.
// Eight args because this verb takes the most flags of the four; grouping them into a struct
// would just move the same fields behind one more name.
#[allow(clippy::too_many_arguments)]
pub async fn clone_create_with_new_ticket(
    client: &Client,
    team: &str,
    title: &str,
    description: &str,
    agent_instructions: Option<&String>,
    claude_instructions: Option<&String>,
    common: &CreateArgs,
    json: bool,
) -> Result<u8> {
    let http = reqwest::Client::new();
    let team = team.trim().to_ascii_lowercase();
    let cfg = client.config().await?;
    // The team key IS the preset choice: it is matched against the presets' own ticket-id
    // prefixes, the same rule that auto-selects one for an existing ticket.
    let preset = crate::linear::pick_preset_by_prefix(&cfg.presets, &team).ok_or_else(|| {
        anyhow!(
            "no preset claims team {}. Add it to a preset's ticket-id prefixes (configured: {})",
            team.to_uppercase(),
            preset_names(&cfg),
        )
    })?;
    let issue =
        crate::linear::create_issue(&http, &preset.linear_key, &team, title.trim(), description)
            .await?;
    if let Err(e) = crate::linear::ensure_in_progress(&http, &preset.linear_key, &issue).await {
        eprintln!("warning: could not move {} to In Progress: {e}", issue.identifier);
    }
    let op = client
        .clone_create(
            &common.from,
            linear_mode(&issue),
            &clone_opts(common, None, agent_instructions, claude_instructions),
        )
        .await?;
    started(client, op, &common.wait, json, "clone").await
}

fn preset_names(cfg: &wire::AppConfigRedacted) -> String {
    cfg.presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// `rmng clone create-plain --title <t>` — no-ticket clone with a title-derived hostname.
pub async fn clone_create_plain(
    client: &Client,
    title: &str,
    message: &str,
    preset: Option<&str>,
    common: &CreateArgs,
    json: bool,
) -> Result<u8> {
    let op = client
        .clone_create(
            &common.from,
            json!({ "plain": { "title": title.trim(), "message": message } }),
            &clone_opts(common, preset, None, None),
        )
        .await?;
    started(client, op, &common.wait, json, "clone").await
}

pub async fn clone_rm(
    client: &Client,
    clone: &str,
    yes: bool,
    wait: &WaitArgs,
    json: bool,
) -> Result<u8> {
    if !yes {
        use std::io::{BufRead, IsTerminal, Write};
        if !std::io::stdin().is_terminal() {
            bail!("refusing to destroy '{clone}' non-interactively without -y/--yes");
        }
        eprint!("destroy clone '{clone}'? this removes its container and volumes [y/N] ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            eprintln!("aborted");
            return Ok(1);
        }
    }
    let op = client.delete(clone).await?;
    started(client, op, wait, json, "delete").await
}

pub async fn archive(client: &Client, clone: &str, wait: &WaitArgs, json: bool) -> Result<u8> {
    let op = client.archive(clone).await?;
    started(client, op, wait, json, "archive").await
}

pub async fn restore(client: &Client, clone: &str, wait: &WaitArgs, json: bool) -> Result<u8> {
    let op = client.unarchive(clone).await?;
    started(client, op, wait, json, "restore").await
}

/// `rmng account swap <clone> <account> [--codex]` — hot-swap a clone's account for one
/// provider. `account` is a selection verbatim: an email, `auto`, `none`, or `group:<pool>`.
/// The token is installed into the clone's credential file immediately — no restart.
pub async fn account_swap(
    client: &Client,
    clone: &str,
    account: &str,
    codex: bool,
    json: bool,
) -> Result<u8> {
    let account = account.trim();
    if account.is_empty() {
        bail!("provide an account: an email, `auto`, `none`, or `group:<pool>`");
    }
    let reply = if codex {
        client.codex_swap(clone, account).await?
    } else {
        client.claude_swap(clone, account).await?
    };
    if json {
        emit_json(&reply)?;
    } else {
        let provider = if codex { "codex" } else { "claude" };
        println!("set {clone} {provider} account → {account}");
    }
    Ok(0)
}

/// `rmng account rm <account> [--codex]` — delete an imported account by email. The server
/// refuses if a clone is explicitly PINNED to it (an operator choice, not a rotation), and
/// otherwise moves every clone that was running it onto another account.
pub async fn account_rm(client: &Client, account: &str, codex: bool, json: bool) -> Result<u8> {
    let reply = if codex {
        client.codex_delete(account).await?
    } else {
        client.claude_delete(account).await?
    };
    if json {
        emit_json(&reply)?;
    } else {
        let moved = reply
            .get("moved")
            .and_then(|m| m.as_array())
            .map(|m| m.len())
            .unwrap_or(0);
        println!("removed {account} ({moved} clone(s) moved)");
    }
    Ok(0)
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
        ImageCmd::Commit {
            clone,
            as_name,
            wait,
        } => {
            let op = client.image_commit(clone, as_name).await?;
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
        AccountCmd::Swap { clone, account, codex } => {
            account_swap(client, clone, account, *codex, json).await
        }
        AccountCmd::Rm { account, codex } => account_rm(client, account, *codex, json).await,
        AccountCmd::Ls { provider } => {
            let st = client.state().await?;
            // `claude_accounts` holds BOTH providers' rows, tagged by `provider`.
            let accounts: Vec<_> = st
                .claude_accounts
                .iter()
                .filter(|account| match provider {
                    None => true,
                    Some(CliProvider::Claude) => {
                        matches!(account.provider, Some(Provider::Claude) | None)
                    }
                    Some(CliProvider::Codex) => matches!(account.provider, Some(Provider::Codex)),
                })
                .collect();
            if json {
                emit_json(&accounts)?;
                return Ok(0);
            }
            let rows: Vec<Vec<String>> = accounts
                .iter()
                .map(|account| {
                    vec![
                        account.email.clone(),
                        match account.provider {
                            Some(Provider::Codex) => "codex".into(),
                            _ => "claude".into(),
                        },
                        account
                            .assignable
                            .map(|assignable| if assignable { "yes" } else { "no" }.to_string())
                            .unwrap_or_default(),
                        pct(&account.five_hour),
                        account
                            .five_hour
                            .as_ref()
                            .and_then(|window| window.resets_at.clone())
                            .unwrap_or_default(),
                        pct(&account.seven_day),
                        pct(&account.fable),
                        account.error.clone().unwrap_or_default(),
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
                        "ERROR",
                    ],
                    &rows,
                )
            );
            Ok(0)
        }
    }
}

pub async fn op_ls(client: &Client, json: bool) -> Result<u8> {
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

// --- the transcript ledger ---------------------------------------------------

/// Milliseconds since the Unix epoch, now.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A `--since`/`--until` bound as epoch milliseconds.
///
/// Two forms, because the two questions are different. "What happened in the last two days" is
/// a duration ago (`90m`, `6h`, `2d`, `3w`), which is what you actually type. An exact instant
/// is epoch milliseconds, which is what a script already holds and what the API takes.
///
/// `now` is a parameter so the duration arithmetic is testable without a clock.
fn parse_when(raw: &str, now: i64) -> Result<i64> {
    let s = raw.trim();
    if s.is_empty() {
        bail!("empty time bound");
    }
    let (digits, unit) = s.split_at(s.len() - 1);
    let per_unit = match unit {
        "m" => 60_000i64,
        "h" => 60 * 60_000,
        "d" => 24 * 60 * 60_000,
        "w" => 7 * 24 * 60 * 60_000,
        // No suffix: epoch milliseconds, verbatim.
        _ => {
            return s
                .parse::<i64>()
                .map_err(|_| anyhow!("'{raw}' is neither a duration (90m, 6h, 2d, 3w) nor epoch millis"));
        }
    };
    let n: i64 = digits
        .parse()
        .map_err(|_| anyhow!("'{raw}' is neither a duration (90m, 6h, 2d, 3w) nor epoch millis"))?;
    Ok(now - n.saturating_mul(per_unit))
}

/// `rmng ledger search <pattern>` — the matching lines, newest first.
pub async fn ledger_search(
    client: &Client,
    pattern: &str,
    clone: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    limit: usize,
    json: bool,
) -> Result<u8> {
    let now = now_ms();
    let since = since.map(|s| parse_when(s, now)).transpose()?;
    let until = until.map(|s| parse_when(s, now)).transpose()?;
    let found = client
        .ledger_search(pattern, clone, since, until, Some(limit))
        .await?;
    if json {
        emit_json(&found)?;
        return Ok(0);
    }
    if found.hits.is_empty() {
        eprintln!("no ledger line matches '{pattern}'");
        return Ok(0);
    }
    // The session and offset are columns rather than a footnote: they are the two arguments
    // `ledger read` takes, so a hit worth following up is already a command you can copy.
    let rows: Vec<Vec<String>> = found
        .hits
        .iter()
        .map(|h| {
            vec![
                h.clone.clone(),
                h.ts.clone(),
                h.kind.clone(),
                h.session.clone(),
                h.offset.to_string(),
                truncate(&record_text(&h.line), 80),
            ]
        })
        .collect();
    print!(
        "{}",
        table(&["CLONE", "WHEN", "KIND", "SESSION", "OFFSET", "TEXT"], &rows)
    );
    if found.truncated {
        eprintln!(
            "stopped at {} hits; there are more matches (raise --limit, or narrow with --clone/--since)",
            found.hits.len()
        );
    }
    Ok(0)
}

/// The `text` of a serialized ledger record, with its newlines flattened so one record stays
/// one table row. A line that will not parse is shown raw rather than dropped.
fn record_text(line: &str) -> String {
    let text = serde_json::from_str::<wire::LedgerRecord>(line)
        .map(|r| r.text)
        .unwrap_or_else(|_| line.to_string());
    text.replace('\n', " ⏎ ")
}

/// `rmng ledger read <clone> <session>` — the raw NDJSON, for piping into `jq`.
pub async fn ledger_read(
    client: &Client,
    clone: &str,
    session: &str,
    offset: u64,
    len: u64,
    json: bool,
) -> Result<u8> {
    let range = client.ledger_read(clone, session, offset, len).await?;
    if json {
        emit_json(&range)?;
        return Ok(0);
    }
    // The text is already whole NDJSON lines, which is the pipeable thing. The envelope goes to
    // stderr so `rmng ledger read … | jq` works without a flag.
    print!("{}", range.text);
    eprintln!(
        "{clone}/{session}: bytes {}..{} of {}",
        range.offset,
        range.offset + range.len,
        range.size
    );
    Ok(0)
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
                "{verb} started: op {} target {} (follow with `rmng op wait {}`)",
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
                "timed out after {timeout}s waiting for op {op_id} (it may still be running — check `rmng op ls`)"
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

fn validate_ssh_host<'a>(st: &'a ControlState, host: &str) -> Result<&'a wire::RmngClone> {
    let target = st
        .hosts
        .iter()
        .find(|candidate| candidate.id == host)
        .ok_or_else(|| anyhow!("unknown clone '{host}' (see `rmng clone ls`)"))?;
    if !target.managed {
        bail!("'{host}' is not a managed clone; RMNG has no SSH endpoint for it")
    }
    if target.archived {
        bail!("clone '{host}' is archived; restore it first")
    }
    if matches!(target.monitor_state, Some(MonitorState::Offline)) {
        bail!("clone '{host}' is offline; its SSH endpoint is unavailable")
    }
    Ok(target)
}

/// True when this `rmng` is running INSIDE a clone. Every clone carries its per-clone router
/// bearer in `RMNG_PROXY_KEY` (`provision::router_env_vars`); an operator laptop does not. We
/// reuse that same identity signal the server already trusts — no server round-trip or
/// peer-IP needed — to decide clone→clone (direct) vs operator→clone (bastion) SSH.
fn running_inside_clone() -> bool {
    std::env::var("RMNG_PROXY_KEY").map(|v| !v.trim().is_empty()).unwrap_or(false)
}

/// The direct one-liner used clone→clone: no bastion jump — clones share the `rmng` Docker
/// bridge and reach each other by internal IP / Docker-DNS id. `accept-new` trusts the target's
/// stable host key on first contact, and the `rmng@` user is spelled out here rather than assumed
/// from config.
///
/// This prints a command; it does not make it work. The server no longer provisions a shared
/// client identity (see `control-server`'s `ssh::clone_ssh_tar_entries`), so unless the user has
/// set up their own key — added to the target's `authorized_keys` via the operator key list, or
/// loaded in an agent — this will prompt or be refused. That is deliberate: the old shared key
/// came with a `Host *` block that hijacked `User`/`IdentityFile` for every destination.
pub fn build_direct_ssh_command(target: &str) -> String {
    format!("ssh -o StrictHostKeyChecking=accept-new rmng@{target}")
}

/// The address a sibling clone dials: its internal bridge IP when known, else its id (Docker
/// DNS resolves the id to the same host, so this is safe when a fresh clone isn't IP-sampled).
fn direct_ssh_target(host: &wire::RmngClone) -> String {
    host.local_ip.clone().unwrap_or_else(|| host.id.clone())
}

/// `rmng ssh <clone>`: print the ready-to-paste `ssh` one-liner that jumps through the
/// bastion into the clone. Fetches the redacted config for `ssh.publicHost` and
/// `listen.bastion`; falls back to a best-effort host guess (with a stderr note) when
/// `publicHost` isn't set, so the command on stdout stays copy-pasteable either way.
pub async fn clone_ssh(client: &Client, clone: &str, json: bool) -> Result<u8> {
    let st = client.state().await?;
    let target = validate_ssh_host(&st, clone)?;

    // From inside a clone: skip the bastion entirely and dial the sibling directly over the
    // shared Docker bridge. Prefer its internal IP; fall back to the clone id (Docker DNS
    // resolves it) when a just-started clone hasn't been IP-sampled yet.
    let (command, mode) = if running_inside_clone() {
        (build_direct_ssh_command(&direct_ssh_target(target)), "direct")
    } else {
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
        (build_ssh_command(&public_host, cfg.listen.bastion, clone), "bastion")
    };
    if json {
        emit_json(&serde_json::json!({ "command": command, "mode": mode }))?;
    } else {
        println!("{command}");
    }
    Ok(0)
}

/// What a desktop verb does with the daemon's `content` array once it comes back.
enum Kind {
    /// `monitors`/`windows`: print the JSON text result, no screenshot.
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
/// `$TMPDIR/rmng-<clone>-mon<N>.jpg`), and return its absolute path. The daemon has already
/// sized the JPEG to the call's coordinate space (`--resolution` / `--native`), so this is a
/// straight decode-and-write with no client-side resampling.
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

    // `--resolution` / `--native`, resolved once and forwarded verbatim: the daemon owns the
    // scaling, so this one value fixes both the space `x`/`y` are read in AND the size of every
    // image the call produces (the verb's own settle shot or the auto-snap below) — they cannot
    // drift apart. `None` means "daemon default" and is dropped from the args by `args_obj`.
    // Verbs that carry no coordinates or image (Monitors/Windows/Key/Type/MoveWindow) don't
    // flatten the flags at all, hence the `_ => None` arm.
    let resolution: Option<String> = match cmd {
        DesktopCmd::Screenshot { resolution, .. }
        | DesktopCmd::Move { resolution, .. }
        | DesktopCmd::Click { resolution, .. }
        | DesktopCmd::RightClick { resolution, .. }
        | DesktopCmd::MiddleClick { resolution, .. }
        | DesktopCmd::DoubleClick { resolution, .. }
        | DesktopCmd::Scroll { resolution, .. } => {
            resolution.resolution_arg().map_err(anyhow::Error::msg)?
        }
        _ => None,
    };
    let res = || resolution.clone().map(Value::from).unwrap_or(Value::Null);

    // (tool, args, kind, monitor-for-screenshots, out path)
    let (tool, args, kind, monitor, out): (&str, Value, Kind, Option<u32>, Option<PathBuf>) =
        match cmd {
            DesktopCmd::Screenshot {
                monitor, out, ..
            } => (
                "screenshot",
                args_obj(vec![("monitor", n(*monitor)), ("resolution", res())]),
                Kind::Screenshot,
                *monitor,
                out.clone(),
            ),
            DesktopCmd::Monitors => ("list_monitors", args_obj(vec![]), Kind::Query, None, None),
            DesktopCmd::Windows => ("list_windows", args_obj(vec![]), Kind::Query, None, None),
            // X/Y go to the daemon untouched: they are already in the space the daemon reports
            // and screenshots in (`resolution`, or its 1080p default).
            DesktopCmd::Move {
                x, y, monitor, out, ..
            } => (
                "mouse_move",
                args_obj(vec![
                    ("x", (*x).into()),
                    ("y", (*y).into()),
                    ("monitor", n(*monitor)),
                    ("resolution", res()),
                ]),
                Kind::Action,
                *monitor,
                out.clone(),
            ),
            DesktopCmd::Click {
                x, y, monitor, out, ..
            } => (
                "left_click",
                args_obj(vec![
                    ("x", i(*x)),
                    ("y", i(*y)),
                    ("monitor", n(*monitor)),
                    ("resolution", res()),
                ]),
                Kind::Action,
                *monitor,
                out.clone(),
            ),
            DesktopCmd::RightClick {
                x, y, monitor, out, ..
            } => (
                "right_click",
                args_obj(vec![
                    ("x", i(*x)),
                    ("y", i(*y)),
                    ("monitor", n(*monitor)),
                    ("resolution", res()),
                ]),
                Kind::Action,
                *monitor,
                out.clone(),
            ),
            DesktopCmd::MiddleClick {
                x, y, monitor, out, ..
            } => (
                "middle_click",
                args_obj(vec![
                    ("x", i(*x)),
                    ("y", i(*y)),
                    ("monitor", n(*monitor)),
                    ("resolution", res()),
                ]),
                Kind::Action,
                *monitor,
                out.clone(),
            ),
            DesktopCmd::DoubleClick {
                x, y, monitor, out, ..
            } => (
                "left_double_click",
                args_obj(vec![
                    ("x", i(*x)),
                    ("y", i(*y)),
                    ("monitor", n(*monitor)),
                    ("resolution", res()),
                ]),
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
                ..
            } => (
                "scroll",
                args_obj(vec![
                    ("amount", (*amount).into()),
                    ("x", i(*x)),
                    ("y", i(*y)),
                    ("monitor", n(*monitor)),
                    ("resolution", res()),
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
            DesktopCmd::MoveWindow { id, monitor, mode } => (
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
            if json {
                emit_json(&serde_json::json!({ "screenshot": path.display().to_string() }))?;
            } else {
                println!("{}", path.display());
            }
            Ok(0)
        }
        Kind::Action => {
            let text = content_text(&content);
            // Guarantee a settle screenshot: reuse the action's own image if it has
            // one, else make a follow-up `screenshot` call.
            let shot = if content_image(&content).is_some() {
                content
            } else {
                // Same `resolution` as the action, so the follow-up image is in the space the
                // caller's next coordinates will be read in.
                client
                    .desktop(
                        clone,
                        "screenshot",
                        args_obj(vec![("monitor", n(monitor)), ("resolution", res())]),
                    )
                    .await?
            };
            let path = write_screenshot(&shot, clone, monitor, out.as_deref())?;
            if json {
                emit_json(&serde_json::json!({
                    "screenshot": path.display().to_string(),
                    "text": text,
                }))?;
            } else {
                if !text.is_empty() {
                    println!("{text}");
                }
                println!("{}", path.display());
            }
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
    detach: bool,
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
    // Detached execs return no output and take no stdin (nothing is attached), so skip the drain.
    let stdin_b64 = if detach || std::io::stdin().is_terminal() || !stdin_has_input(STDIN_POLL_GRACE)
    {
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
        detach,
    };
    let result = client.exec(clone, &req).await?;

    if json {
        emit_json(&result)?;
    } else if !detach {
        // Detached: the server returns an empty result immediately — nothing to print.
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

    /// A fixed clock, so the duration arithmetic is checked rather than the machine's time.
    const NOW: i64 = 1_785_600_000_000;

    #[test]
    fn a_time_bound_is_a_duration_ago_or_epoch_millis() {
        assert_eq!(parse_when("90m", NOW).unwrap(), NOW - 90 * 60_000);
        assert_eq!(parse_when("6h", NOW).unwrap(), NOW - 6 * 60 * 60_000);
        assert_eq!(parse_when("2d", NOW).unwrap(), NOW - 2 * 24 * 60 * 60_000);
        assert_eq!(parse_when("3w", NOW).unwrap(), NOW - 3 * 7 * 24 * 60 * 60_000);
        // No suffix is an instant, verbatim, which is what a script already holds.
        assert_eq!(parse_when("1785578400000", NOW).unwrap(), 1_785_578_400_000);
        assert_eq!(parse_when(" 2d ", NOW).unwrap(), NOW - 2 * 24 * 60 * 60_000);
    }

    #[test]
    fn a_time_bound_that_means_nothing_is_refused_rather_than_guessed() {
        for bad in ["", "  ", "soon", "2y", "-d", "2 d"] {
            assert!(parse_when(bad, NOW).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn a_hit_shows_its_records_text_on_one_row() {
        let line = r#"{"clone":"c1","session":"s1","ts":"2026-08-01T10:00:00.000Z","kind":"user","text":"fix\nthe encoder"}"#;
        assert_eq!(record_text(line), "fix ⏎ the encoder");
        // A line that will not parse is shown as it is rather than dropped: a ledger the
        // format has moved on from is still evidence.
        assert_eq!(record_text("not json"), "not json");
    }

    #[test]
    fn ssh_command_is_the_inline_jump_one_liner() {
        assert_eq!(
            build_ssh_command("rmng.example.com", 2222, "w-cp-claude"),
            "ssh -J rmng@rmng.example.com:2222 -o StrictHostKeyChecking=accept-new rmng@w-cp-claude"
        );
    }

    #[test]
    fn direct_ssh_command_skips_the_bastion() {
        assert_eq!(
            build_direct_ssh_command("172.20.0.5"),
            "ssh -o StrictHostKeyChecking=accept-new rmng@172.20.0.5"
        );
    }

    #[test]
    fn direct_ssh_target_prefers_local_ip_then_falls_back_to_id() {
        let with_ip = wire::RmngClone {
            id: "clone-b".into(),
            local_ip: Some("172.20.0.9".into()),
            ..Default::default()
        };
        assert_eq!(direct_ssh_target(&with_ip), "172.20.0.9");

        let no_ip = wire::RmngClone {
            id: "clone-b".into(),
            local_ip: None,
            ..Default::default()
        };
        assert_eq!(direct_ssh_target(&no_ip), "clone-b");
    }

    #[test]
    fn validate_ssh_host_rejects_unknown_host() {
        let st = ControlState {
            hosts: vec![wire::RmngClone {
                id: "pega-herms".into(),
                managed: true,
                ..Default::default()
            }],
            ..Default::default()
        };

        let err = validate_ssh_host(&st, "herms").expect_err("clone suffix must not match");
        assert_eq!(err.to_string(), "unknown clone 'herms' (see `rmng clone ls`)");
        validate_ssh_host(&st, "pega-herms").expect("exact clone id should match");
    }

    #[test]
    fn validate_ssh_host_rejects_unreachable_targets() {
        let st = ControlState {
            hosts: vec![
                wire::RmngClone {
                    id: "legacy".into(),
                    ..Default::default()
                },
                wire::RmngClone {
                    id: "archived".into(),
                    managed: true,
                    archived: true,
                    ..Default::default()
                },
                wire::RmngClone {
                    id: "offline".into(),
                    managed: true,
                    monitor_state: Some(MonitorState::Offline),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        assert_eq!(
            validate_ssh_host(&st, "legacy").unwrap_err().to_string(),
            "'legacy' is not a managed clone; RMNG has no SSH endpoint for it"
        );
        assert_eq!(
            validate_ssh_host(&st, "archived").unwrap_err().to_string(),
            "clone 'archived' is archived; restore it first"
        );
        assert_eq!(
            validate_ssh_host(&st, "offline").unwrap_err().to_string(),
            "clone 'offline' is offline; its SSH endpoint is unavailable"
        );
    }

    #[test]
    fn validate_ssh_host_allows_active_or_unsampled_clones() {
        let st = ControlState {
            hosts: [None, Some(MonitorState::Working), Some(MonitorState::Idle)]
                .into_iter()
                .enumerate()
                .map(|(index, monitor_state)| wire::RmngClone {
                    id: format!("clone-{index}"),
                    managed: true,
                    monitor_state,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };

        for host in ["clone-0", "clone-1", "clone-2"] {
            validate_ssh_host(&st, host).expect("managed clone should have an SSH command");
        }
    }

    #[test]
    fn ps_formatters_handle_metrics_and_status() {
        assert_eq!(cpu_pct(0.4), "0.4%");
        assert_eq!(cpu_pct(18.4), "18%");
        assert_eq!(
            ram(&ContainerStats {
                cpu_pct: 0.0,
                mem_used: 2 * 1024 * 1024 * 1024,
                mem_limit: 4 * 1024 * 1024 * 1024,
            }),
            "2.0 GiB/4.0 GiB"
        );
        assert_eq!(clone_status(true, Some(MonitorState::Working)), "archived");
        assert_eq!(clone_status(false, Some(MonitorState::Idle)), "idle");
        assert_eq!(clone_status(false, None), "");
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
