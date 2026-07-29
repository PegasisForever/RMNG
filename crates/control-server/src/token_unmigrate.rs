//! One-shot, stamp-gated startup migration **out of** the per-group CLIProxyAPI `auth-dir`
//! credential files (`<data_dir>/cliproxy/<group>/auth/<type>-<email>.json`) and back into the
//! RMNG-owned OAuth token stores (`<data_dir>/claude-accounts.json` + `codex-accounts.json`,
//! plus the `cloneGroups`/`codexGroups` lists in `./config.json`).
//!
//! This is the exact inverse of the retired `token_migrate.rs`, and exists so that reverting
//! the group-proxy architecture carries every account across with **no operator re-login**.
//! Under the restored model RMNG owns each account's refresh lifecycle again and injects only
//! a short-lived access token into each clone (see [`crate::claude`] / [`crate::codex`]).
//!
//! Runs once, guarded by its own `<data_dir>/.token-unmigration-done` stamp. Note this is a
//! DIFFERENT stamp from the forward migration's `.token-migration-done`, which may still be
//! present on an upgraded deployment and means the opposite thing — keying off that one would
//! make this migration a no-op exactly where it is needed.
//!
//! It is security-sensitive (real OAuth tokens):
//!   - both stores are written `0600`, the stamp `0600`;
//!   - **no** `access_token` / `refresh_token` / `id_token` value is ever logged — only counts,
//!     emails, and group names.
//!
//! Each account lands in **exactly one** store entry. A single-use refresh token must never
//! live in two places: two stores refreshing the same token would invalidate each other. The
//! same email can legitimately appear in several groups' auth-dirs (each instance held its own
//! independent token set), so the FIRST occurrence in group order wins and the rest are dropped
//! with a warning. Group membership is reconstructed from the directory each winning account
//! was found in.
//!
//! Antigravity (Gemini) accounts are **not** carried across: that provider only ever existed
//! through CLIProxyAPI and has no credential-injection path. Their files are left untouched in
//! the auth-dir and reported as a count, so the operator knows those logins were left behind
//! rather than silently discarded.

use std::collections::{BTreeMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app::App;

/// Presence of this file (relative to `data_dir`) means the reverse migration already ran.
/// Deliberately distinct from the forward migration's `.token-migration-done`.
const STAMP: &str = ".token-unmigration-done";
const CLAUDE_STORE: &str = "claude-accounts.json";
const CODEX_STORE: &str = "codex-accounts.json";
/// The per-group instance root the forward migration wrote into: `<data_dir>/cliproxy/<group>/auth/`.
const CLIPROXY_DIR: &str = "cliproxy";

// --- the auth-dir file shape we must read -----------------------------------------------

/// One CLIProxyAPI `auth-dir` credential file.
///
/// This deliberately does NOT reuse the retired `cliproxy::AuthAccount`, which kept only
/// `{kind, email, access_token, account_id}` because the usage poller needed nothing else.
/// The reverse migration additionally needs `refresh_token` (the whole point — it is what RMNG
/// takes ownership of), `id_token` (Codex sends it in `~/.codex/auth.json`), and `expired` (the
/// token's expiry, as RFC3339).
#[derive(Deserialize, Default)]
struct RawAuthFile {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    /// RFC3339 expiry as CLIProxyAPI writes it. Absent/unparseable ⇒ 0, which reads as
    /// "already expired" and forces a refresh on first use — the safe direction.
    #[serde(default)]
    expired: Option<String>,
}

/// A credential recovered from one auth-dir file, plus the group it was found in.
struct Recovered {
    kind: String,
    email: String,
    group: String,
    access_token: String,
    refresh_token: String,
    id_token: String,
    account_id: String,
    expires_at: i64,
}

// --- the old store shapes we must write -------------------------------------------------

/// Old `claude-accounts.json` entry. Mirrors `claude::StoredClaudeAccount` — kept as its own
/// local type so this migration stays readable as a pure data transform, and so a later change
/// to the live struct can't silently alter what gets written here.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct ClaudeAccount {
    id: String,
    email: String,
    org_uuid: String,
    org_name: String,
    active: bool,
    access_token: String,
    refresh_token: String,
    /// Epoch **milliseconds** (the auth-dir stores RFC3339 seconds).
    expires_at: i64,
    scopes: Vec<String>,
}

/// Old `codex-accounts.json` entry. Mirrors `codex::StoredCodexAccount`.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct CodexAccount {
    id: String,
    email: String,
    account_id: String,
    plan: String,
    active: bool,
    access_token: String,
    id_token: String,
    refresh_token: String,
    expires_at: i64,
}

#[derive(Serialize, Default)]
struct ClaudeStoreFile {
    accounts: Vec<ClaudeAccount>,
}

#[derive(Serialize, Default)]
struct CodexStoreFile {
    accounts: Vec<CodexAccount>,
}

/// The scopes a Claude OAuth subscription login carries. The auth-dir file does not record
/// them, and the old store's consumers only ever read them back out verbatim, so seeding the
/// standard pair keeps the restored store self-consistent.
fn default_claude_scopes() -> Vec<String> {
    vec!["user:inference".to_string(), "user:profile".to_string()]
}

// --- parsing ----------------------------------------------------------------------------

/// Parse an RFC3339 timestamp (as `docker::epoch_to_rfc3339` emits it — `YYYY-MM-DDTHH:MM:SSZ`)
/// back to epoch **milliseconds**, the unit both old stores use for `expiresAt`.
///
/// Reuses `claude`'s hand-rolled parser, so this accepts exactly the forms the forward
/// migration could have written (and the fractional-second / offset forms the providers emit).
/// `None` for an absent or unparseable value; the caller maps that to `0` ⇒ treated as expired
/// ⇒ refreshed on first use, which is the safe direction to fail in.
fn expiry_ms(expired: Option<&str>) -> Option<i64> {
    let raw = expired?.trim();
    if raw.is_empty() {
        return None;
    }
    crate::claude::parse_rfc3339_utc_secs(raw).map(|secs| secs * 1000)
}

/// Parse one auth-dir credential file body + its file name into a [`Recovered`]. `kind` comes
/// from the JSON `type` when present, else the file-name prefix — matching how CLIProxyAPI and
/// the forward migration both wrote them. `None` when there is no access token, no usable
/// email, or no refresh token (an entry RMNG could not take ownership of).
fn parse_auth_file(file_name: &str, body: &str, group: &str) -> Option<Recovered> {
    let raw: RawAuthFile = serde_json::from_str(body).ok()?;
    let access_token = raw.access_token.filter(|t| !t.is_empty())?;
    // Without a refresh token RMNG cannot own the lifecycle, and an access token alone expires
    // within the hour — carrying it across would produce a clone that breaks silently.
    let refresh_token = raw.refresh_token.filter(|t| !t.is_empty())?;
    let kind = raw.r#type.filter(|t| !t.is_empty()).unwrap_or_else(|| {
        if file_name.starts_with("codex-") {
            "codex".to_string()
        } else if file_name.starts_with("antigravity-") {
            "antigravity".to_string()
        } else {
            "claude".to_string()
        }
    });
    // Prefer the JSON email; fall back to the `<kind>-<email>.json` file-name stem.
    let email = raw
        .email
        .filter(|e| !e.is_empty())
        .or_else(|| {
            file_name
                .strip_suffix(".json")
                .and_then(|s| s.split_once('-').map(|(_, e)| e.to_string()))
                .filter(|e| !e.is_empty())
        })?;
    Some(Recovered {
        kind,
        email,
        group: group.to_string(),
        access_token,
        refresh_token,
        id_token: raw.id_token.unwrap_or_default(),
        account_id: raw.account_id.unwrap_or_default(),
        expires_at: expiry_ms(raw.expired.as_deref()).unwrap_or(0),
    })
}

/// Enumerate every group directory under `<data_dir>/cliproxy/` and the credentials in each
/// one's `auth/`. Returned sorted by (group, file name) so the "first occurrence wins" dedup
/// below is deterministic rather than dependent on readdir order.
fn scan_auth_dirs(data_dir: &Path) -> Vec<Recovered> {
    let root = data_dir.join(CLIPROXY_DIR);
    let Ok(groups) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut group_names: Vec<String> = groups
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    group_names.sort();

    let mut out = Vec::new();
    for group in group_names {
        let auth_dir = root.join(&group).join("auth");
        let Ok(entries) = std::fs::read_dir(&auth_dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        files.sort();
        for path in files {
            let file_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(rec) = parse_auth_file(&file_name, &body, &group) {
                out.push(rec);
            }
        }
    }
    out
}

/// Keep the FIRST occurrence of each `(kind, email)` and drop the rest, warning per drop.
///
/// This is the load-bearing safety rule: a refresh token is single-use, so the same account
/// appearing in two groups' auth-dirs must not produce two store entries — two independent
/// refreshes of one token invalidate each other and log the operator out of the account.
fn dedupe(recovered: Vec<Recovered>) -> Vec<Recovered> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out = Vec::new();
    for rec in recovered {
        let key = (rec.kind.clone(), rec.email.clone());
        if seen.insert(key) {
            out.push(rec);
        } else {
            tracing::warn!(
                target: "token_unmigrate",
                "{} account {} appears in more than one group (also {}); keeping the first \
                 occurrence only — a single-use refresh token must live in exactly one store",
                rec.kind, rec.email, rec.group,
            );
        }
    }
    out
}

// --- writing ----------------------------------------------------------------------------

/// Serialize `body` to `path` with `0600` permissions, via a temp file + rename so a crash
/// mid-write can't leave a truncated token store behind.
fn write_store(path: &Path, body: &impl Serialize) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut bytes = serde_json::to_vec_pretty(body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    bytes.push(b'\n');
    std::fs::write(&tmp, &bytes)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn write_stamp(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, b"unmigrated\n")?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Repoint every clone's account binding, from the RAW `state.json`, onto the restored model.
///
/// Under the group proxy a clone carried ONE `group: "<pool>"`; the restored model has a
/// selection per provider. That key no longer exists on `RmngClone`, so serde drops it at load
/// and the clone ends up with `claude_selection: None` — which matches NEITHER rotation path:
/// `rotate_pool` only walks clones with a `claude_group`, and `auto_pool_clones` requires
/// `claude_selection == Some("auto")`. `push_stale_tokens` then skips it too (no email). The
/// clone would sit there, running, with no credentials and nothing logged.
///
/// So each clone is given `group:<pool>` for a provider whose pool survived the migration, and
/// plain `auto` otherwise — either way it lands in a pool the rotator actually walks.
pub type PoolSnapshot = std::collections::HashMap<String, String>;

/// Each clone's `group` from the RAW `state.json`, as it was on disk at process start.
///
/// **Must be called before anything mutates the state store.** `RmngClone.group` no longer
/// exists, so the first `store.mutate` — `web::mirror_layout_to_state`, which runs early in
/// `main` — persists `state.json` WITHOUT it, and the binding is gone from disk before the
/// migration ever looks. That is not hypothetical: it is what happened on the first real
/// migration, where every clone fell back to `auto` instead of its former pool. Harmless where
/// there is one pool; wrong wherever pools hold different accounts.
pub fn read_raw_clone_pools(cfg: &wire::AppConfig) -> PoolSnapshot {
    #[derive(Deserialize, Default)]
    struct RawHost {
        #[serde(default)]
        id: String,
        #[serde(default)]
        group: String,
    }
    #[derive(Deserialize, Default)]
    struct RawState {
        #[serde(default)]
        hosts: Vec<RawHost>,
    }
    let path = crate::config::state_path(cfg);
    let raw: RawState = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    raw.hosts.into_iter().filter(|h| !h.id.is_empty()).map(|h| (h.id, h.group)).collect()
}

fn heal_clone_bindings(
    app: &App,
    was: &PoolSnapshot,
    claude_pools: &[wire::CloneGroup],
    codex_pools: &[wire::CloneGroup],
) {

    let mut bound = 0usize;
    let mut autos = 0usize;
    app.store.mutate(|s| {
        for h in s.hosts.iter_mut() {
            if !h.managed || h.claude_selection.is_some() || h.codex_selection.is_some() {
                continue; // already on the restored model — never clobber a real selection
            }
            let pool = was.get(&h.id).map(String::as_str).unwrap_or("");
            let pick = |pools: &[wire::CloneGroup]| -> String {
                if !pool.is_empty() && pools.iter().any(|g| g.name == pool) {
                    format!("group:{pool}")
                } else {
                    "auto".to_string()
                }
            };
            let claude = pick(claude_pools);
            let codex = pick(codex_pools);
            if claude.starts_with("group:") || codex.starts_with("group:") {
                bound += 1;
            } else {
                autos += 1;
            }
            if claude.starts_with("group:") {
                h.claude_group = Some(pool.to_string());
            }
            if codex.starts_with("group:") {
                h.codex_group = Some(pool.to_string());
            }
            h.claude_selection = Some(claude);
            h.codex_selection = Some(codex);
        }
    });
    if bound + autos > 0 {
        tracing::info!(
            target: "token_unmigrate",
            "repointed {} clone(s) at their former pool and {} at `auto` — a clone left with no \
             selection matches neither rotation path and would never be given a token",
            bound, autos,
        );
    }
}

/// Each preset's `group` from the RAW `./config.json`.
///
/// `Preset.group` was removed with the group-proxy model, so the live `AppConfig` no longer
/// deserializes it — by the time this migration runs, `app.config()` has already dropped it.
/// Reading the file directly is the only way to see what a preset was bound to, and it must
/// happen before the first config save rewrites the file without it.
fn read_raw_preset_groups() -> std::collections::HashMap<String, String> {
    #[derive(Deserialize, Default)]
    struct RawPreset {
        #[serde(default)]
        name: String,
        #[serde(default)]
        group: String,
    }
    #[derive(Deserialize, Default)]
    struct RawCfg {
        #[serde(default)]
        presets: Vec<RawPreset>,
    }
    let path = crate::config::config_path();
    let raw: RawCfg = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    raw.presets
        .into_iter()
        .filter(|p| !p.name.is_empty())
        .map(|p| (p.name, p.group))
        .collect()
}

/// Group the recovered accounts of one provider into the old `CloneGroup` list shape: one
/// entry per source group directory, holding the emails found in it, sorted for a stable
/// `config.json`.
fn rebuild_groups(recovered: &[Recovered], kind: &str) -> Vec<wire::CloneGroup> {
    let mut by_group: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rec in recovered.iter().filter(|r| r.kind == kind) {
        by_group
            .entry(rec.group.clone())
            .or_default()
            .push(rec.email.clone());
    }
    by_group
        .into_iter()
        .map(|(name, mut accounts)| {
            accounts.sort();
            wire::CloneGroup { name, accounts }
        })
        .collect()
}

// --- entry point ------------------------------------------------------------------------

/// One-shot, stamp-gated migration of the per-group `auth-dir` credentials back into the RMNG
/// token stores. Best-effort: every failure is logged, none blocks boot. Call once at startup,
/// AFTER config load and BEFORE the account pollers are spawned, so the stores exist by the
/// time the first poll reads them.
///
/// `pools_before` must come from [`read_raw_clone_pools`] called BEFORE any state mutation —
/// see that function for why reading it here would be too late.
pub fn unmigrate_group_proxy_tokens(app: &App, pools_before: &PoolSnapshot) {
    let data_dir = app.config().data_dir.clone();
    let data_path = PathBuf::from(&data_dir);
    let stamp = data_path.join(STAMP);

    // 1. Gate. Already reverse-migrated → done.
    if stamp.exists() {
        return;
    }
    // No auth-dirs at all → a fresh install or a deployment that never onboarded an account.
    // Cheap no-op each boot; deliberately NOT stamped, so a `/data` volume attached later
    // still migrates.
    if !data_path.join(CLIPROXY_DIR).exists() {
        return;
    }

    // 2. Recover every credential, then enforce one-store-per-account.
    let recovered = dedupe(scan_auth_dirs(&data_path));
    let antigravity = recovered.iter().filter(|r| r.kind == "antigravity").count();

    let claude: Vec<ClaudeAccount> = recovered
        .iter()
        .filter(|r| r.kind == "claude")
        .map(|r| ClaudeAccount {
            // The old id is `{email}|{org_uuid}`; the auth-dir never recorded an org, so the
            // uuid half is empty. Stable and unique per email, which is all the store needs.
            id: format!("{}|", r.email),
            email: r.email.clone(),
            org_uuid: String::new(),
            org_name: String::new(),
            active: false,
            access_token: r.access_token.clone(),
            refresh_token: r.refresh_token.clone(),
            expires_at: r.expires_at,
            scopes: default_claude_scopes(),
        })
        .collect();

    let codex: Vec<CodexAccount> = recovered
        .iter()
        .filter(|r| r.kind == "codex")
        .map(|r| CodexAccount {
            id: format!("codex:{}", r.account_id),
            email: r.email.clone(),
            account_id: r.account_id.clone(),
            plan: String::new(),
            active: false,
            access_token: r.access_token.clone(),
            id_token: r.id_token.clone(),
            refresh_token: r.refresh_token.clone(),
            expires_at: r.expires_at,
        })
        .collect();

    if claude.is_empty() && codex.is_empty() {
        // Nothing to carry across. Stamp anyway when the auth-dirs existed but held nothing
        // usable, so this doesn't rescan every boot forever.
        if antigravity > 0 {
            tracing::warn!(
                target: "token_unmigrate",
                "{antigravity} Antigravity (Gemini) account(s) found but NOT migrated — that \
                 provider only existed through CLIProxyAPI and has no credential-injection \
                 path; re-add those accounts under Claude or Codex if you need them",
            );
        }
        if let Err(e) = write_stamp(&stamp) {
            tracing::warn!(target: "token_unmigrate", "writing stamp failed: {e}");
        }
        tracing::info!(target: "token_unmigrate", "no group-proxy credentials to carry across");
        return;
    }

    // 3. Write both stores (0600). A failure here leaves the stamp absent so the next boot
    //    retries — the auth-dir files are only ever read, never consumed.
    let claude_count = claude.len();
    let codex_count = codex.len();
    if !claude.is_empty() {
        let path = data_path.join(CLAUDE_STORE);
        if let Err(e) = write_store(&path, &ClaudeStoreFile { accounts: claude }) {
            tracing::error!(target: "token_unmigrate", "writing {} failed: {e}; will retry next boot", path.display());
            return;
        }
    }
    if !codex.is_empty() {
        let path = data_path.join(CODEX_STORE);
        if let Err(e) = write_store(&path, &CodexStoreFile { accounts: codex }) {
            tracing::error!(target: "token_unmigrate", "writing {} failed: {e}; will retry next boot", path.display());
            return;
        }
    }

    // 4. Rebuild the two group lists from the source directories and persist them.
    let clone_groups = rebuild_groups(&recovered, "claude");
    let codex_groups = rebuild_groups(&recovered, "codex");
    let mut cfg = app.config();
    cfg.clone_groups = clone_groups;
    cfg.codex_groups = codex_groups;
    // Carry each preset's pool binding across. The group-proxy era had ONE provider-agnostic
    // `Preset.group`; the restored model has one default per provider, so a preset that pointed
    // at `Personal` now defaults BOTH providers to `group:Personal` — the closest thing to what
    // it meant, and only where that pool actually survived the migration.
    //
    // Read from the RAW config: `Preset.group` no longer exists as a field, so serde has already
    // dropped it from `app.config()` by the time we get here. Without this, every preset on an
    // upgraded deployment silently loses its binding and its clones fall through to `auto`.
    let raw_preset_pools = read_raw_preset_groups();
    let mut carried = 0usize;
    for preset in cfg.presets.iter_mut() {
        let Some(pool) = raw_preset_pools.get(&preset.name).filter(|p| !p.is_empty()) else {
            continue;
        };
        let sel = format!("group:{pool}");
        if preset.claude_account.is_empty() && cfg.clone_groups.iter().any(|g| &g.name == pool) {
            preset.claude_account = sel.clone();
            carried += 1;
        }
        if preset.codex_account.is_empty() && cfg.codex_groups.iter().any(|g| &g.name == pool) {
            preset.codex_account = sel;
            carried += 1;
        }
    }
    if let Err(e) = crate::config::save(&cfg) {
        tracing::error!(target: "token_unmigrate", "saving config with restored groups failed: {e:#}; will retry next boot");
        return;
    }
    if carried > 0 {
        tracing::info!(
            target: "token_unmigrate",
            "carried {carried} preset pool default(s) across (one per provider whose pool survived)",
        );
    }

    // Pull what we just wrote into the LIVE stores. `App::new` loaded both files long before
    // this ran, so without this the process keeps the pre-migration snapshot — and the first
    // refresh persists that stale snapshot back over the credentials recovered above, while the
    // stamp guarantees the migration never retries. Silent, permanent, and the whole reason the
    // recovery would otherwise be worthless.
    app.claude.reload_from_disk();
    app.codex.reload_from_disk();

    // Clone rows carry the same dead `group` key; heal them against the pools that survived.
    heal_clone_bindings(app, pools_before, &cfg.clone_groups, &cfg.codex_groups);

    let group_names: Vec<String> = cfg
        .clone_groups
        .iter()
        .chain(cfg.codex_groups.iter())
        .map(|g| g.name.clone())
        .collect();
    *app.cfg.write().unwrap() = cfg;

    // 5. Stamp + summary (never log token values).
    if let Err(e) = write_stamp(&stamp) {
        tracing::warn!(target: "token_unmigrate", "writing stamp failed: {e}; migration may re-run next boot");
    }
    if antigravity > 0 {
        tracing::warn!(
            target: "token_unmigrate",
            "{antigravity} Antigravity (Gemini) account(s) found but NOT migrated — that \
             provider only existed through CLIProxyAPI and has no credential-injection path",
        );
    }
    tracing::info!(
        target: "token_unmigrate",
        "group-proxy token reverse-migration complete: {claude_count} claude + {codex_count} \
         codex account(s) restored; groups {group_names:?}",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: &str, email: &str, group: &str) -> Recovered {
        Recovered {
            kind: kind.into(),
            email: email.into(),
            group: group.into(),
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            id_token: String::new(),
            account_id: String::new(),
            expires_at: 0,
        }
    }

    #[test]
    fn parses_claude_and_codex_auth_shapes() {
        let claude = parse_auth_file(
            "claude-a@b.com.json",
            r#"{"id_token":"","access_token":"AT","refresh_token":"RT","email":"a@b.com",
                "type":"claude","expired":"2021-01-01T00:00:00Z"}"#,
            "team",
        )
        .unwrap();
        assert_eq!(claude.kind, "claude");
        assert_eq!(claude.email, "a@b.com");
        assert_eq!(claude.access_token, "AT");
        assert_eq!(claude.refresh_token, "RT");
        assert_eq!(claude.group, "team");
        // RFC3339 seconds → epoch MILLIseconds, the unit the old stores use.
        assert_eq!(claude.expires_at, 1_609_459_200_000);

        let codex = parse_auth_file(
            "codex-c@d.com.json",
            r#"{"id_token":"IDT","access_token":"AT2","refresh_token":"RT2","email":"c@d.com",
                "type":"codex","account_id":"acc-1","expired":"2021-01-01T00:00:00Z"}"#,
            "team",
        )
        .unwrap();
        assert_eq!(codex.kind, "codex");
        assert_eq!(codex.id_token, "IDT");
        assert_eq!(codex.account_id, "acc-1");
    }

    #[test]
    fn kind_falls_back_to_the_file_name_prefix() {
        let body = r#"{"access_token":"AT","refresh_token":"RT","email":"a@b.com"}"#;
        assert_eq!(parse_auth_file("codex-a@b.com.json", body, "g").unwrap().kind, "codex");
        assert_eq!(
            parse_auth_file("antigravity-a@b.com.json", body, "g").unwrap().kind,
            "antigravity"
        );
        assert_eq!(parse_auth_file("claude-a@b.com.json", body, "g").unwrap().kind, "claude");
        // Email falls back to the file-name stem when the JSON omits it.
        let no_email = r#"{"access_token":"AT","refresh_token":"RT"}"#;
        assert_eq!(parse_auth_file("claude-x@y.com.json", no_email, "g").unwrap().email, "x@y.com");
    }

    /// A credential RMNG cannot take ownership of must be skipped, not half-migrated: without
    /// a refresh token the access token expires within the hour and the clone breaks silently.
    #[test]
    fn rejects_entries_without_a_refresh_or_access_token() {
        let no_refresh = r#"{"access_token":"AT","email":"a@b.com","type":"claude"}"#;
        assert!(parse_auth_file("claude-a@b.com.json", no_refresh, "g").is_none());
        let empty_refresh =
            r#"{"access_token":"AT","refresh_token":"","email":"a@b.com","type":"claude"}"#;
        assert!(parse_auth_file("claude-a@b.com.json", empty_refresh, "g").is_none());
        let no_access = r#"{"refresh_token":"RT","email":"a@b.com","type":"claude"}"#;
        assert!(parse_auth_file("claude-a@b.com.json", no_access, "g").is_none());
        assert!(parse_auth_file("claude-a@b.com.json", "{ not json", "g").is_none());
    }

    /// A missing or unparseable expiry must read as 0 (= already expired ⇒ refreshed on first
    /// use), never as a far-future value that would let a dead token reach a clone.
    #[test]
    fn missing_expiry_reads_as_expired() {
        assert_eq!(expiry_ms(None), None);
        assert_eq!(expiry_ms(Some("")), None);
        assert_eq!(expiry_ms(Some("not-a-date")), None);
        assert_eq!(expiry_ms(Some("2021-01-01T00:00:00Z")), Some(1_609_459_200_000));
        let parsed = parse_auth_file(
            "claude-a@b.com.json",
            r#"{"access_token":"AT","refresh_token":"RT","email":"a@b.com"}"#,
            "g",
        )
        .unwrap();
        assert_eq!(parsed.expires_at, 0);
    }

    /// The load-bearing guard: one refresh token, one store entry. Two entries would let two
    /// refreshes race and invalidate each other, logging the operator out of the account.
    #[test]
    fn dedupe_keeps_the_first_occurrence_per_provider_and_email() {
        let out = dedupe(vec![
            rec("claude", "a@b.com", "alpha"),
            rec("claude", "a@b.com", "beta"), // same account, second group → dropped
            rec("codex", "a@b.com", "alpha"), // same email, DIFFERENT provider → kept
            rec("claude", "c@d.com", "beta"),
        ]);
        assert_eq!(out.len(), 3);
        let claude: Vec<_> = out.iter().filter(|r| r.kind == "claude").collect();
        assert_eq!(claude.len(), 2);
        // The surviving a@b.com row is the one from the first group, in sorted group order.
        assert_eq!(claude[0].email, "a@b.com");
        assert_eq!(claude[0].group, "alpha");
        assert_eq!(out.iter().filter(|r| r.kind == "codex").count(), 1);
    }

    /// The pool snapshot must be read from disk BEFORE anything rewrites `state.json`.
    ///
    /// This is a regression test for a real production miss: `web::mirror_layout_to_state` runs
    /// early in `main` and persists the state store, and since `RmngClone` no longer has a
    /// `group` field, that write drops it. By the time the migration looked, every clone read as
    /// unbound and fell back to `auto` — logged as "repointed 0 clone(s) at their former pool".
    /// Harmless on a single-pool deployment; wrong wherever pools hold different accounts.
    #[test]
    fn raw_pool_snapshot_reads_the_pre_migration_state_file() {
        let dir = std::env::temp_dir().join(format!("rmng-poolsnap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = wire::AppConfig {
            data_dir: dir.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let path = crate::config::state_path(&cfg);

        // A `state.json` as the group-proxy era wrote it: `group` per clone, no account fields.
        std::fs::write(
            &path,
            r#"{"hosts":[
                {"id":"a","host":"a","managed":true,"group":"Personal"},
                {"id":"b","host":"b","managed":true,"group":"Medi"},
                {"id":"c","host":"c","managed":true}
            ]}"#,
        )
        .unwrap();
        let before = read_raw_clone_pools(&cfg);
        assert_eq!(before.get("a").map(String::as_str), Some("Personal"));
        assert_eq!(before.get("b").map(String::as_str), Some("Medi"));
        assert_eq!(before.get("c").map(String::as_str), Some(""), "no group is empty, not absent");

        // Now simulate what the early state-store write does: the same file, minus `group`.
        std::fs::write(
            &path,
            r#"{"hosts":[
                {"id":"a","host":"a","managed":true},
                {"id":"b","host":"b","managed":true}
            ]}"#,
        )
        .unwrap();
        let after = read_raw_clone_pools(&cfg);
        assert_eq!(after.get("a").map(String::as_str), Some(""), "the binding is gone from disk");
        // Which is exactly why the snapshot must be taken first — it still holds the truth.
        assert_eq!(before.get("a").map(String::as_str), Some("Personal"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A preset's pool binding must survive the migration.
    ///
    /// Modelled on real production data (CT 105): four presets, every one carrying a `group`,
    /// and 41 clones bound to those pools. `Preset.group` no longer exists as a field, so serde
    /// drops it before the migration sees `app.config()` — reading the raw file is the only way
    /// to recover it, and without that every preset on an upgraded deployment silently loses its
    /// binding.
    ///
    /// This pins the pure mapping the migration applies; `read_raw_preset_groups` itself is a
    /// thin file read.
    /// The migration's output must reach the RUNNING process, not just the disk.
    ///
    /// `App::new` loads both token stores at startup; the migration runs long after. Without a
    /// reload the process keeps the pre-migration snapshot — and because every store mutation
    /// persists the whole in-memory vector (`ClaudeStore::save`), the first token refresh writes
    /// that stale snapshot back over the recovered credentials. The stamp then prevents any
    /// retry. On the production boxes this would have meant running on week-old, already-rotated
    /// refresh tokens while the log said the migration succeeded.
    #[test]
    fn reload_replaces_the_snapshot_app_new_loaded() {
        let dir = std::env::temp_dir().join(format!("rmng-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CLAUDE_STORE);

        // The state at boot: one stale account, as `App::new` would have read it.
        std::fs::write(
            &path,
            r#"{"accounts":[{"id":"old|","email":"stale@x.com","accessToken":"OLD",
                "refreshToken":"OLDR","expiresAt":1}]}"#,
        )
        .unwrap();
        let store = crate::claude::ClaudeStore::load(dir.to_str().unwrap());
        assert!(store.get_by_email("stale@x.com").is_some());

        // What the migration writes afterwards: the recovered set, which does NOT contain the
        // stale account and DOES contain one the boot snapshot never had.
        write_store(
            &path,
            &ClaudeStoreFile {
                accounts: vec![ClaudeAccount {
                    id: "fresh@x.com|".into(),
                    email: "fresh@x.com".into(),
                    access_token: "NEW".into(),
                    refresh_token: "NEWR".into(),
                    expires_at: 2,
                    scopes: default_claude_scopes(),
                    ..Default::default()
                }],
            },
        )
        .unwrap();

        // Before the reload the process is still on the boot snapshot — this is the bug.
        assert!(store.get_by_email("fresh@x.com").is_none());
        store.reload_from_disk();
        // After it, the recovered account is live and the stale one is gone, so a refresh can no
        // longer persist the old set back over the new file.
        let fresh = store.get_by_email("fresh@x.com").expect("recovered account not adopted");
        assert_eq!(fresh.access_token, "NEW");
        assert!(store.get_by_email("stale@x.com").is_none(), "stale account survived the reload");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preset_pool_binding_maps_to_both_providers() {
        // (raw group, claude pools that survived, codex pools that survived) → what a preset gets.
        let apply = |pool: &str, claude_pools: &[&str], codex_pools: &[&str]| -> (String, String) {
            let mut claude = String::new();
            let mut codex = String::new();
            let sel = format!("group:{pool}");
            if claude_pools.contains(&pool) {
                claude = sel.clone();
            }
            if codex_pools.contains(&pool) {
                codex = sel;
            }
            (claude, codex)
        };

        // The common case: the pool holds both providers' accounts, so both default to it.
        assert_eq!(
            apply("Personal", &["Personal", "Medi"], &["Personal"]),
            ("group:Personal".into(), "group:Personal".into())
        );
        // A pool with only Claude accounts binds only the Claude side — pointing Codex at a pool
        // with no Codex credentials in it would be a dangling selection.
        assert_eq!(
            apply("Medi", &["Personal", "Medi"], &["Personal"]),
            ("group:Medi".into(), String::new())
        );
        // A pool that did not survive the migration at all binds neither.
        assert_eq!(apply("Gone", &["Personal"], &["Personal"]), (String::new(), String::new()));
    }

    #[test]
    fn rebuild_groups_partitions_by_provider_and_sorts() {
        let recovered = vec![
            rec("claude", "b@x.com", "team"),
            rec("claude", "a@x.com", "team"),
            rec("claude", "c@x.com", "beta"),
            rec("codex", "z@o.com", "team"),
        ];
        let claude = rebuild_groups(&recovered, "claude");
        assert_eq!(claude.len(), 2);
        // BTreeMap ⇒ groups sorted by name; emails sorted within each.
        assert_eq!(claude[0].name, "beta");
        assert_eq!(claude[0].accounts, vec!["c@x.com"]);
        assert_eq!(claude[1].name, "team");
        assert_eq!(claude[1].accounts, vec!["a@x.com", "b@x.com"]);
        // Providers never bleed into each other's list.
        let codex = rebuild_groups(&recovered, "codex");
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].accounts, vec!["z@o.com"]);
        assert!(rebuild_groups(&recovered, "antigravity").is_empty());
    }

    #[test]
    fn scans_every_group_dir_in_sorted_order_and_skips_junk() {
        let root = std::env::temp_dir().join(format!("rmng-unmig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (group, file, body) in [
            ("beta", "claude-b@x.com.json", r#"{"access_token":"A","refresh_token":"R","email":"b@x.com","type":"claude"}"#),
            ("alpha", "claude-a@x.com.json", r#"{"access_token":"A","refresh_token":"R","email":"a@x.com","type":"claude"}"#),
            ("alpha", "notes.txt", "ignore me"),
            ("alpha", "broken.json", "{ not json"),
        ] {
            let dir = root.join(CLIPROXY_DIR).join(group).join("auth");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(file), body).unwrap();
        }
        let got = scan_auth_dirs(&root);
        assert_eq!(got.len(), 2, "non-json and unparseable files are skipped");
        // Sorted by group name, so dedup's "first wins" is deterministic across runs.
        assert_eq!(got[0].group, "alpha");
        assert_eq!(got[1].group, "beta");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_cliproxy_dir_scans_to_nothing() {
        let root = std::env::temp_dir().join(format!("rmng-unmig-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(scan_auth_dirs(&root).is_empty());
    }

    #[test]
    fn stores_are_written_0600_with_the_old_camelcase_shape() {
        let dir = std::env::temp_dir().join(format!("rmng-unmig-w-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CLAUDE_STORE);
        write_store(
            &path,
            &ClaudeStoreFile {
                accounts: vec![ClaudeAccount {
                    id: "a@b.com|".into(),
                    email: "a@b.com".into(),
                    access_token: "AT".into(),
                    refresh_token: "RT".into(),
                    expires_at: 1_609_459_200_000,
                    scopes: default_claude_scopes(),
                    ..Default::default()
                }],
            },
        )
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a token store must never be world-readable");
        let body = std::fs::read_to_string(&path).unwrap();
        // camelCase keys — the restored `claude.rs` deserializes exactly these.
        assert!(body.contains("\"accessToken\": \"AT\""));
        assert!(body.contains("\"refreshToken\": \"RT\""));
        assert!(body.contains("\"expiresAt\": 1609459200000"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
