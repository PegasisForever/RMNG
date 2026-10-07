//! `rmng clone create <kind>`: the four tabs of the dashboard's "New clone" dialog.
//!
//! `template` builds from a preset image (`POST /api/clone`); the other three fork a clone
//! (`POST /api/fork`). The two ticket kinds first do in Linear what the dialog does in the
//! browser ([`crate::linear`]), then hand the issue to the fork as `linear`. The preset and
//! the clone to fork are picked here, by the dialog's own defaults ([`default_preset`],
//! [`default_source`]), so `--dry-run` shows them. The rest of what a flag leaves out, the
//! server fills in from the preset, the same as for the dialog.
//!
//! The checks that can fail run before the first write to Linear, so a typo does not leave
//! a ticket behind with no clone.

use anyhow::{Result, anyhow, bail};
use control_client::Client;
use serde_json::{Value, json};

use crate::args::{CreateArgs, CreateCmd, KickoffArgs, SourceArgs, read_text};
use crate::commands::{column_id, start_clone, start_clone_in};
use crate::linear::{self, Issue, Linear, NewIssue};

/// `--message` / `--message-file`, as the request carries it: absent when neither was given.
pub fn first_message(
    inline: Option<&String>,
    file: Option<&std::path::PathBuf>,
) -> Result<Option<String>> {
    let body = read_text(inline, file)?;
    Ok(Some(body).filter(|b| !b.trim().is_empty()))
}

/// The request fields every create verb shares.
pub fn request(c: &CreateArgs) -> wire::CloneRequest {
    wire::CloneRequest {
        group: c.group.clone(),
        claude_account: c.claude_account.clone(),
        codex_account: c.codex_account.clone(),
        headless: c.headless,
        rebuild: c.rebuild,
        run_startup_script: !c.no_startup_script,
        ..Default::default()
    }
}

fn titled(title: &str) -> Option<wire::LinearMeta> {
    Some(wire::LinearMeta {
        display_name: Some(title.to_string()),
        ..Default::default()
    })
}

pub async fn create(client: &Client, cmd: &CreateCmd, json: bool) -> Result<u8> {
    match cmd {
        CreateCmd::Template {
            title,
            preset,
            message,
            common,
        } => {
            let first = first_message(message.message.as_ref(), message.message_file.as_ref())?;
            let cfg = client.config().await?;
            let req = wire::CloneRequest {
                preset: default_preset(&cfg.presets, preset.as_deref())?,
                linear: titled(title),
                kickoff: first.is_some(),
                first_message: first,
                ..request(common)
            };
            send(client, false, req, common, json, None).await
        }
        CreateCmd::NoTicket {
            title,
            preset,
            message,
            source,
            common,
        } => {
            let first = first_message(message.message.as_ref(), message.message_file.as_ref())?;
            let cfg = client.config().await?;
            let preset = default_preset(&cfg.presets, preset.as_deref())?;
            let source = fork_source(client, &cfg.presets, preset.as_deref(), source).await?;
            let req = wire::CloneRequest {
                source: source.source,
                parent: source.parent,
                preset,
                // A title of its own: the fork does not keep its source's ticket.
                linear: titled(title),
                kickoff: first.is_some(),
                first_message: first,
                ..request(common)
            };
            send(client, true, req, common, json, None).await
        }
        CreateCmd::Ticket {
            ticket,
            preset,
            source,
            kickoff,
            common,
        } => {
            let r = linear::parse_ticket_ref(ticket).map_err(|e| anyhow!(e))?;
            let column = column_id(client, common.column.as_deref()).await?;
            let cfg = client.config().await?;
            let preset = pick_preset(&cfg.presets, preset.as_deref(), &r.prefix)?;
            let source = &fork_source(client, &cfg.presets, preset.as_deref(), source).await?;
            let lin = Linear::new();
            let keys = linear::keys_for_team(&cfg.presets, &r.prefix);
            let (issue, key) = lin.find(&keys, &r).await.map_err(|e| anyhow!(e))?;
            let ctx = TicketCtx {
                issue,
                key,
                opened: false,
            };
            fork_for_ticket(
                client, &lin, ctx, preset, source, kickoff, common, column, json,
            )
            .await
        }
        CreateCmd::NewTicket {
            title,
            team,
            description,
            description_file,
            priority,
            assignee,
            preset,
            source,
            kickoff,
            common,
        } => {
            let description = read_text(description.as_ref(), description_file.as_ref())?;
            let column = column_id(client, common.column.as_deref()).await?;
            let cfg = client.config().await?;
            let team = pick_team(&cfg.presets, team.as_deref())?;
            let preset = pick_preset(&cfg.presets, preset.as_deref(), &team)?;
            let source = &fork_source(client, &cfg.presets, preset.as_deref(), source).await?;
            let key = linear::keys_for_team(&cfg.presets, &team)
                .into_iter()
                .next()
                .ok_or_else(|| {
                    anyhow!("no preset has a Linear API key (add one in Settings → Presets)")
                })?;
            let lin = Linear::new();
            let assignee_id = match assignee {
                Some(who) => {
                    let (people, viewer) = lin.people(&key, &team).await.map_err(|e| anyhow!(e))?;
                    Some(
                        linear::pick_person(&people, &viewer, who)
                            .map_err(|e| anyhow!(e))?
                            .id
                            .clone(),
                    )
                }
                None => None,
            };
            let new = NewIssue {
                team: team.clone(),
                title: title.clone(),
                description,
                priority: priority.map(|p| p.linear()),
                assignee_id,
            };
            if common.dry_run {
                // The ticket does not exist yet, so the request carries only its title.
                let req = wire::CloneRequest {
                    linear: titled(title),
                    ..ticket_request(preset, source, kickoff, common)
                };
                let ticket = json!({
                    "team": team.to_ascii_uppercase(),
                    "title": new.title,
                    "priority": new.priority,
                    "assigneeId": new.assignee_id,
                    "opened": false,
                });
                return dry_run(true, &req, Some(ticket));
            }
            let issue = lin.create(&key, &new).await.map_err(|e| anyhow!(e))?;
            eprintln!("opened {}: {}", issue.identifier, issue.url);
            let ctx = TicketCtx {
                issue,
                key,
                opened: true,
            };
            fork_for_ticket(
                client, &lin, ctx, preset, source, kickoff, common, column, json,
            )
            .await
        }
    }
}

/// The issue a ticket verb resolved, with the key proven to write it.
struct TicketCtx {
    issue: Issue,
    key: String,
    /// This run opened it, so a failed fork leaves it behind.
    opened: bool,
}

/// The fork request for a ticket verb, before the ticket itself is added.
fn ticket_request(
    preset: Option<String>,
    source: &SourceArgs,
    kickoff: &KickoffArgs,
    common: &CreateArgs,
) -> wire::CloneRequest {
    let trimmed = |s: &Option<String>| {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    wire::CloneRequest {
        source: source.source.clone(),
        parent: source.parent.clone(),
        preset,
        agent_instructions: trimmed(&kickoff.agent_instructions),
        claude_instructions: trimmed(&kickoff.claude_instructions),
        kickoff: !kickoff.no_kickoff,
        ..request(common)
    }
}

/// Move the ticket to In Progress (best effort, like the dialog), then fork a clone for it.
#[allow(clippy::too_many_arguments)]
async fn fork_for_ticket(
    client: &Client,
    lin: &Linear,
    t: TicketCtx,
    preset: Option<String>,
    source: &SourceArgs,
    kickoff: &KickoffArgs,
    common: &CreateArgs,
    column: Option<String>,
    json: bool,
) -> Result<u8> {
    let id = &t.issue.identifier;
    let req = wire::CloneRequest {
        linear: Some(linear::clone_meta(&t.issue)),
        ..ticket_request(preset, source, kickoff, common)
    };
    let mut ticket: Value = json!({
        "identifier": id,
        "title": t.issue.title,
        "url": t.issue.url,
        "opened": t.opened,
    });
    if common.dry_run {
        ticket["movesToInProgress"] = json!(t.issue.state_type != "started");
        return dry_run(true, &req, Some(ticket));
    }
    match lin.start(&t.key, &t.issue).await {
        Ok(true) => eprintln!("moved {id} to In Progress"),
        Ok(false) => {}
        Err(e) => eprintln!("warning: could not move {id} to In Progress: {e}"),
    }
    let result = start_clone_in(
        client,
        true,
        req,
        column.as_deref(),
        &common.wait,
        json,
        Some(ticket),
    )
    .await;
    match result {
        Err(e) if t.opened => Err(e.context(format!(
            "{id} was opened ({}) but no clone started for it; to try again without a second \
             ticket, run `rmng clone create ticket {id}`",
            t.issue.url
        ))),
        r => r,
    }
}

/// Start the clone, or with `--dry-run` only print the request. `--column` is checked
/// either way.
pub async fn send(
    client: &Client,
    fork: bool,
    req: wire::CloneRequest,
    common: &CreateArgs,
    json: bool,
    ticket: Option<Value>,
) -> Result<u8> {
    if common.dry_run {
        column_id(client, common.column.as_deref()).await?;
        return dry_run(fork, &req, ticket);
    }
    start_clone(client, fork, req, common, json, ticket).await
}

/// What `--dry-run` prints: the route, the body, and the ticket. Always JSON: it is the
/// request itself, so there is no table to draw.
fn dry_run(fork: bool, req: &wire::CloneRequest, ticket: Option<Value>) -> Result<u8> {
    let mut v = json!({
        "dryRun": true,
        "route": if fork { "POST /api/fork" } else { "POST /api/clone" },
        "request": req,
    });
    if let Some(t) = ticket {
        v["ticket"] = t;
    }
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(0)
}

/// The preset a ticket verb uses: the one named, else the one labelled with the team. With
/// presets configured and none labelled, it is refused, as the dialog refuses it.
fn pick_preset(
    presets: &[wire::PresetRedacted],
    named: Option<&str>,
    team: &str,
) -> Result<Option<String>> {
    let names = || {
        presets
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(name) = named.map(str::trim).filter(|n| !n.is_empty()) {
        if !presets.iter().any(|p| p.name == name) {
            bail!("unknown preset '{name}' (configured: {})", names());
        }
        return Ok(Some(name.to_string()));
    }
    if let Some(p) = linear::preset_for_team(presets, team) {
        return Ok(Some(p.name.clone()));
    }
    if presets.is_empty() {
        return Ok(None);
    }
    bail!(
        "no preset has the label '{}'; pass --preset <name> (configured: {})",
        team.to_ascii_uppercase(),
        names()
    )
}

/// The team a new ticket opens in: the one named, else the only one. Only teams a preset
/// is labelled with, which are the ones the dialog offers.
fn pick_team(presets: &[wire::PresetRedacted], named: Option<&str>) -> Result<String> {
    let teams = linear::teams(presets);
    let list = || {
        teams
            .iter()
            .map(|t| t.to_ascii_uppercase())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if teams.is_empty() {
        bail!(
            "no preset names a Linear team: add the team key as a preset label in Settings → Presets"
        );
    }
    match named.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) if teams.contains(&t.to_ascii_lowercase()) => Ok(t.to_ascii_lowercase()),
        Some(t) => bail!(
            "no preset has the label '{}' (teams: {})",
            t.to_ascii_uppercase(),
            list()
        ),
        None if teams.len() == 1 => Ok(teams[0].clone()),
        None => bail!("pass --team (teams: {})", list()),
    }
}

/// `rmng clone fork`: a copy that keeps its source's ticket and, unless `--preset` names
/// another, its preset. The source is picked as [`default_source`] picks it.
#[allow(clippy::too_many_arguments)]
pub async fn fork(
    client: &Client,
    source: Option<&String>,
    parent: Option<&String>,
    preset: Option<&String>,
    message: Option<&String>,
    message_file: Option<&std::path::PathBuf>,
    common: &CreateArgs,
    json: bool,
) -> Result<u8> {
    let first = first_message(message, message_file)?;
    let cfg = client.config().await?;
    let preset = match preset.map(|p| p.trim()).filter(|p| !p.is_empty()) {
        Some(name) => default_preset(&cfg.presets, Some(name))?,
        None => None,
    };
    let picked = SourceArgs {
        source: source.cloned(),
        parent: parent.cloned(),
    };
    let picked = fork_source(client, &cfg.presets, preset.as_deref(), &picked).await?;
    let req = wire::CloneRequest {
        source: picked.source,
        parent: picked.parent,
        preset,
        // No `linear`: the copy keeps its source's ticket and is named after it.
        kickoff: first.is_some(),
        first_message: first,
        ..request(common)
    };
    send(client, true, req, common, json, None).await
}

/// The preset the dialog's "No ticket" and "From template" tabs use: the one named, else
/// the first configured one, which is what those tabs start on. `None` with no presets.
fn default_preset(presets: &[wire::PresetRedacted], named: Option<&str>) -> Result<Option<String>> {
    match named.map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) if presets.iter().any(|p| p.name == name) => Ok(Some(name.to_string())),
        Some(name) => {
            let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
            bail!("unknown preset '{name}' (configured: {})", names.join(", "))
        }
        None => Ok(presets.first().map(|p| p.name.clone())),
    }
}

/// The clone the dialog forks when nobody picks one (`follow` in
/// `frontend/app/lib/cloneDraft.ts`): the preset's default fork clone where a managed clone
/// still has that id, else the first managed clone in the server's list, live ones before
/// archived ones. The list is newest first, so that is the newest live clone.
pub fn default_source(
    hosts: &[wire::RmngClone],
    presets: &[wire::PresetRedacted],
    preset: Option<&str>,
) -> Result<String> {
    let sources: Vec<&str> = hosts
        .iter()
        .filter(|h| h.managed && !h.archived)
        .chain(hosts.iter().filter(|h| h.managed && h.archived))
        .map(|h| h.id.as_str())
        .collect();
    let Some(first) = sources.first() else {
        bail!("there is no clone to fork; build one with `rmng clone create template`");
    };
    let wanted = preset
        .and_then(|name| presets.iter().find(|p| p.name == name))
        .map(|p| p.default_fork_clone.trim())
        .filter(|d| sources.contains(d));
    Ok(wanted.unwrap_or(first).to_string())
}

/// `--source` and `--parent` as the request sends them: both checked to exist (a typo must
/// not cost a Linear write), and the source filled in by [`default_source`] when omitted.
async fn fork_source(
    client: &Client,
    presets: &[wire::PresetRedacted],
    preset: Option<&str>,
    s: &SourceArgs,
) -> Result<SourceArgs> {
    let st = client.state().await?;
    for id in [&s.source, &s.parent].into_iter().flatten() {
        if !st.hosts.iter().any(|h| &h.id == id) {
            bail!("unknown clone '{id}'");
        }
    }
    let source = match &s.source {
        Some(id) => id.clone(),
        None => default_source(&st.hosts, presets, preset)?,
    };
    Ok(SourceArgs {
        source: Some(source),
        parent: s.parent.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str, labels: &[&str]) -> wire::PresetRedacted {
        wire::PresetRedacted {
            name: name.into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            linear_key: String::new(),
            group: String::new(),
            default_fork_clone: String::new(),
            vars: Vec::new(),
            agent_playbook: String::new(),
            global_prompt: String::new(),
            startup_script: String::new(),
            dockerfile: String::new(),
        }
    }

    fn clone(id: &str, managed: bool, archived: bool) -> wire::RmngClone {
        wire::RmngClone {
            id: id.into(),
            managed,
            archived,
            ..Default::default()
        }
    }

    /// The dialog's pick: the preset's default where a managed clone has that id (archived
    /// counts), else the first live managed clone in the server's (newest first) list.
    #[test]
    fn the_default_source_is_the_dialogs() {
        let hosts = [
            clone("new-archived", true, true),
            clone("unmanaged", false, false),
            clone("newest-live", true, false),
            clone("template", true, true),
            clone("older-live", true, false),
        ];
        let mut p = preset("medi", &["DEV"]);
        p.default_fork_clone = "template".into();
        let presets = [p, preset("other", &[])];
        assert_eq!(
            default_source(&hosts, &presets, Some("medi")).unwrap(),
            "template"
        );
        assert_eq!(
            default_source(&hosts, &presets, Some("other")).unwrap(),
            "newest-live"
        );
        assert_eq!(
            default_source(&hosts, &presets, None).unwrap(),
            "newest-live"
        );
        // A default that names no managed clone falls back.
        let mut gone = preset("gone", &[]);
        gone.default_fork_clone = "unmanaged".into();
        assert_eq!(
            default_source(&hosts, &[gone], Some("gone")).unwrap(),
            "newest-live"
        );
        // Only archived clones: the first of them.
        let archived = [clone("a1", true, true), clone("a2", true, true)];
        assert_eq!(default_source(&archived, &[], None).unwrap(), "a1");
        assert!(default_source(&[clone("u", false, false)], &[], None).is_err());
    }

    #[test]
    fn the_no_ticket_and_template_kinds_start_on_the_first_preset() {
        let presets = [preset("medi", &[]), preset("web", &[])];
        assert_eq!(
            default_preset(&presets, None).unwrap().as_deref(),
            Some("medi")
        );
        assert_eq!(
            default_preset(&presets, Some("web")).unwrap().as_deref(),
            Some("web")
        );
        assert!(default_preset(&presets, Some("nope")).is_err());
        assert_eq!(default_preset(&[], None).unwrap(), None);
    }

    #[test]
    fn a_ticket_takes_the_preset_labelled_with_its_team() {
        let presets = [preset("web", &["WE"]), preset("dev", &["DEV"])];
        assert_eq!(
            pick_preset(&presets, None, "we").unwrap().as_deref(),
            Some("web")
        );
        assert_eq!(
            pick_preset(&presets, Some("dev"), "we").unwrap().as_deref(),
            Some("dev")
        );
        let err = pick_preset(&presets, None, "ng").unwrap_err().to_string();
        assert!(err.contains("'NG'") && err.contains("--preset"), "{err}");
        assert!(pick_preset(&presets, Some("nope"), "we").is_err());
        // No presets at all: the server decides.
        assert_eq!(pick_preset(&[], None, "we").unwrap(), None);
    }

    #[test]
    fn a_new_ticket_opens_in_a_named_team_or_the_only_one() {
        let one = [preset("web", &["WE"])];
        assert_eq!(pick_team(&one, None).unwrap(), "we");
        assert_eq!(pick_team(&one, Some("We")).unwrap(), "we");
        let two = [preset("web", &["WE"]), preset("dev", &["DEV"])];
        let err = pick_team(&two, None).unwrap_err().to_string();
        assert!(err.contains("WE, DEV"), "{err}");
        assert!(pick_team(&two, Some("ng")).is_err());
        assert!(pick_team(&[preset("bare", &[])], None).is_err());
    }
}
