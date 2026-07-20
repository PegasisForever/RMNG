//! One-shot, stamp-gated startup migration from the OLD RMNG-managed OAuth token stores
//! (`<data_dir>/claude-accounts.json` + `codex-accounts.json`, plus the old `./config.json`
//! `cloneGroups`/`codexGroups` lists) into the NEW per-group CLIProxyAPI `auth-dir` credential
//! files (`<data_dir>/cliproxy/<group>/auth/<type>-<email>.json`). Upgrading an existing
//! deployment thus carries every account and its group across with **no operator re-login**.
//!
//! Runs once, guarded by a `<data_dir>/.token-migration-done` stamp. It is security-sensitive
//! (real OAuth tokens):
//!   - auth files are `0600`, the auth-dir is `0700`, the stamp is `0600`;
//!   - **no** `access_token` / `refresh_token` / `id_token` value is ever logged — only counts,
//!     emails, and group names.
//!
//! Each account lands in **exactly one** group (a single-use refresh token must never live in
//! two instances): a Claude account takes the first `cloneGroups` entry that lists its email
//! (else `"Default"`), a Codex account the first `codexGroups` entry (else `"Default"`); the
//! chosen name is sanitized to a `cliproxy::safe_group`-valid form. New groups are
//! provider-agnostic, so an old Claude group `team` and Codex group `team` correctly merge.

use std::collections::{BTreeSet, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;

use crate::app::App;

/// Presence of this file (relative to `data_dir`) means the migration already ran.
const STAMP: &str = ".token-migration-done";
const CLAUDE_STORE: &str = "claude-accounts.json";
const CODEX_STORE: &str = "codex-accounts.json";
/// Grouping fallback when an account is listed in no old group.
const DEFAULT_GROUP: &str = "Default";

// --- OLD on-disk deserialize structs (camelCase; unknown fields ignored) ----------------

#[derive(Deserialize, Default)]
struct ClaudeStore {
    #[serde(default)]
    accounts: Vec<ClaudeAccount>,
}

/// Old `claude-accounts.json` entry. `expiresAt` is epoch **milliseconds**. The store carries
/// no `id_token` (Claude refresh uses `refresh_token`); other fields (orgUuid/scopes/…) are
/// ignored by serde.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ClaudeAccount {
    #[serde(default)]
    email: String,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_at: i64,
}

#[derive(Deserialize, Default)]
struct CodexStore {
    #[serde(default)]
    accounts: Vec<CodexAccount>,
}

/// Old `codex-accounts.json` entry. `expiresAt` is epoch **milliseconds**; other fields
/// (plan/active/…) are ignored by serde.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CodexAccount {
    #[serde(default)]
    email: String,
    #[serde(default)]
    account_id: String,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    id_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_at: i64,
}

/// A single old group: a name + the account emails it holds.
#[derive(Deserialize, Default)]
struct RawOldGroup {
    #[serde(default)]
    name: String,
    #[serde(default)]
    accounts: Vec<String>,
}

/// The two grouping lists still physically present in the old `./config.json` (serde on the
/// live `AppConfig` ignores them now, so we read the raw JSON for them).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawOldConfig {
    #[serde(default)]
    clone_groups: Vec<RawOldGroup>,
    #[serde(default)]
    codex_groups: Vec<RawOldGroup>,
}

// --- grouping ---------------------------------------------------------------------------

/// Coerce an old group name into a `cliproxy::safe_group`-valid form: keep `[A-Za-z0-9._-]`,
/// replace any other char with `-`, truncate to 64 bytes. If the result is empty or still not
/// `safe_group` (e.g. `.` / `..`), fall back to `"Default"`. Every produced char is ASCII, so
/// the byte truncation can never split a multibyte char.
fn sanitize_group(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' })
        .collect();
    s.truncate(64);
    if crate::cliproxy::safe_group(&s) {
        s
    } else {
        DEFAULT_GROUP.to_string()
    }
}

/// The single target group for `email`: the FIRST group in `groups` whose `accounts` list
/// contains the email; if none, `"Default"`. The chosen name is then sanitized.
fn target_group(email: &str, groups: &[RawOldGroup]) -> String {
    let chosen = groups
        .iter()
        .find(|g| g.accounts.iter().any(|a| a == email))
        .map(|g| g.name.as_str())
        .unwrap_or(DEFAULT_GROUP);
    sanitize_group(chosen)
}

// --- auth-file JSON ---------------------------------------------------------------------

/// The NEW CLIProxyAPI auth-file body for a Claude account. `id_token` is empty (the old
/// Claude store has none — Claude refresh uses `refresh_token`); `expired` is the token's
/// expiry (`expiresAt` ms → epoch seconds → RFC3339); `last_refresh` is now (RFC3339).
fn claude_auth_json(acct: &ClaudeAccount, now_rfc: &str) -> serde_json::Value {
    json!({
        "id_token": "",
        "access_token": acct.access_token,
        "refresh_token": acct.refresh_token,
        "last_refresh": now_rfc,
        "email": acct.email,
        "type": "claude",
        "expired": crate::docker::epoch_to_rfc3339(acct.expires_at / 1000),
    })
}

/// The NEW CLIProxyAPI auth-file body for a Codex account: the Claude shape plus `account_id`,
/// `type: "codex"`, and the real `id_token`.
fn codex_auth_json(acct: &CodexAccount, now_rfc: &str) -> serde_json::Value {
    json!({
        "id_token": acct.id_token,
        "access_token": acct.access_token,
        "refresh_token": acct.refresh_token,
        "last_refresh": now_rfc,
        "email": acct.email,
        "type": "codex",
        "account_id": acct.account_id,
        "expired": crate::docker::epoch_to_rfc3339(acct.expires_at / 1000),
    })
}

/// Ensure `auth_dir` exists (`0700`) and write `file_name` inside it (`0600`) with `body`.
/// Returns the written path on success (used for duplicate detection).
fn write_auth_file(
    auth_dir: &Path,
    file_name: &str,
    body: &serde_json::Value,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(auth_dir)?;
    std::fs::set_permissions(auth_dir, std::fs::Permissions::from_mode(0o700))?;
    let path = auth_dir.join(file_name);
    let bytes = serde_json::to_vec_pretty(body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(&path, &bytes)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(path)
}

fn write_stamp(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, b"migrated\n")?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

// --- readers (best-effort: a missing/one file is fine) ----------------------------------

fn read_claude_store(path: &Path) -> Vec<ClaudeAccount> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<ClaudeStore>(&bytes)
            .map(|s| s.accounts)
            .unwrap_or_else(|e| {
                tracing::warn!(target: "token_migrate", "parsing {} failed: {e}", path.display());
                Vec::new()
            }),
        Err(_) => Vec::new(),
    }
}

fn read_codex_store(path: &Path) -> Vec<CodexAccount> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<CodexStore>(&bytes)
            .map(|s| s.accounts)
            .unwrap_or_else(|e| {
                tracing::warn!(target: "token_migrate", "parsing {} failed: {e}", path.display());
                Vec::new()
            }),
        Err(_) => Vec::new(),
    }
}

/// Read the old `./config.json`'s `cloneGroups` / `codexGroups` from the raw JSON (the live
/// `AppConfig` no longer deserializes them). Missing/unparseable → empty lists.
fn read_old_config_groups() -> RawOldConfig {
    let path = crate::config::config_path();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<RawOldConfig>(&bytes).unwrap_or_default(),
        Err(_) => RawOldConfig::default(),
    }
}

fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// --- entry point ------------------------------------------------------------------------

/// One-shot, stamp-gated migration of the legacy token stores into the per-group `auth-dir`
/// credential files. Best-effort: every failure is logged, none blocks boot. Call once at
/// startup, AFTER config load and BEFORE the `cliproxy` supervisor is spawned, so the groups +
/// auth-dirs exist when it starts.
pub fn migrate_legacy_tokens(app: &App) {
    let data_dir = app.config().data_dir.clone();
    let data_path = PathBuf::from(&data_dir);
    let stamp = data_path.join(STAMP);

    // 1. Gate. Already migrated → done. No old store at all → fresh deploy: cheap no-op each
    //    boot, and we deliberately do NOT stamp (so a later legacy store would still migrate).
    if stamp.exists() {
        return;
    }
    let claude_store_path = data_path.join(CLAUDE_STORE);
    let codex_store_path = data_path.join(CODEX_STORE);
    if !claude_store_path.exists() && !codex_store_path.exists() {
        return;
    }

    // 2. Parse the two old stores + the raw old config groups.
    let claude_accounts = read_claude_store(&claude_store_path);
    let codex_accounts = read_codex_store(&codex_store_path);
    let raw_cfg = read_old_config_groups();

    let now_rfc = crate::docker::epoch_to_rfc3339(now_epoch_secs());

    // 3 + 4. Compute each account's single target group and write its auth file.
    let mut used_groups: BTreeSet<String> = BTreeSet::new();
    let mut written: HashSet<PathBuf> = HashSet::new();
    let mut claude_written = 0usize;
    let mut codex_written = 0usize;

    for acct in &claude_accounts {
        let email = acct.email.trim();
        if email.is_empty() || acct.access_token.is_empty() {
            tracing::warn!(target: "token_migrate", "skipping a claude account with no email/token");
            continue;
        }
        let group = target_group(email, &raw_cfg.clone_groups);
        used_groups.insert(group.clone());
        let auth_dir = app.cliproxy.auth_dir(&group);
        let file_name = format!("claude-{email}.json");
        match write_auth_file(&auth_dir, &file_name, &claude_auth_json(acct, &now_rfc)) {
            Ok(path) => {
                if written.insert(path) {
                    claude_written += 1;
                } else {
                    tracing::warn!(target: "token_migrate", "duplicate claude account {email} (group {group}); last wins");
                }
            }
            Err(e) => tracing::warn!(target: "token_migrate", "writing claude auth file for {email} (group {group}) failed: {e}"),
        }
    }

    for acct in &codex_accounts {
        let email = acct.email.trim();
        if email.is_empty() || acct.access_token.is_empty() {
            tracing::warn!(target: "token_migrate", "skipping a codex account with no email/token");
            continue;
        }
        let group = target_group(email, &raw_cfg.codex_groups);
        used_groups.insert(group.clone());
        let auth_dir = app.cliproxy.auth_dir(&group);
        let file_name = format!("codex-{email}.json");
        match write_auth_file(&auth_dir, &file_name, &codex_auth_json(acct, &now_rfc)) {
            Ok(path) => {
                if written.insert(path) {
                    codex_written += 1;
                } else {
                    tracing::warn!(target: "token_migrate", "duplicate codex account {email} (group {group}); last wins");
                }
            }
            Err(e) => tracing::warn!(target: "token_migrate", "writing codex auth file for {email} (group {group}) failed: {e}"),
        }
    }

    // 5. Register every used group in `config.groups` (dedup against existing), persist, write
    //    back into the live config, and allocate ports/keys so the supervisor spawns them.
    let mut cfg = app.config();
    let mut added: Vec<String> = Vec::new();
    for g in &used_groups {
        if !cfg.groups.iter().any(|existing| &existing.name == g) {
            cfg.groups.push(wire::Group { name: g.clone() });
            added.push(g.clone());
        }
    }
    if !added.is_empty() {
        if let Err(e) = crate::config::save(&cfg) {
            // Auth files are already written (idempotent last-wins on re-run); leaving the
            // stamp absent lets the next boot retry the config save safely.
            tracing::error!(target: "token_migrate", "saving config with migrated groups failed: {e:#}; will retry next boot");
            return;
        }
        *app.cfg.write().unwrap() = cfg;
        crate::cliproxy::apply_now(app);
    }

    // 6. Stamp + summary (never log token values).
    if let Err(e) = write_stamp(&stamp) {
        tracing::warn!(target: "token_migrate", "writing migration stamp failed: {e}; migration may re-run next boot");
    }
    let groups: Vec<&str> = used_groups.iter().map(String::as_str).collect();
    tracing::info!(
        target: "token_migrate",
        "legacy token migration complete: {claude_written} claude + {codex_written} codex account(s); groups {groups:?} ({} newly added)",
        added.len(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grp(name: &str, accounts: &[&str]) -> RawOldGroup {
        RawOldGroup { name: name.to_string(), accounts: accounts.iter().map(|s| s.to_string()).collect() }
    }

    /// RFC3339 as `epoch_to_rfc3339` emits it: `YYYY-MM-DDTHH:MM:SSZ` (fixed 20 chars).
    fn is_rfc3339(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 20
            && b[4] == b'-'
            && b[7] == b'-'
            && b[10] == b'T'
            && b[13] == b':'
            && b[16] == b':'
            && b[19] == b'Z'
            && b.iter().enumerate().all(|(i, c)| {
                matches!(i, 4 | 7 | 10 | 13 | 16 | 19) || c.is_ascii_digit()
            })
    }

    #[test]
    fn grouping_single_first_of_multiple_and_default() {
        let clone_groups = vec![
            grp("team", &["a@x.com", "b@x.com"]),
            grp("beta", &["a@x.com"]), // also lists a@ — must NOT win over the first match
        ];
        // Single membership.
        assert_eq!(target_group("b@x.com", &clone_groups), "team");
        // In more than one group → FIRST match only.
        assert_eq!(target_group("a@x.com", &clone_groups), "team");
        // In no group → Default.
        assert_eq!(target_group("nobody@x.com", &clone_groups), "Default");
        // No groups at all → Default.
        assert_eq!(target_group("a@x.com", &[]), "Default");
    }

    #[test]
    fn group_name_sanitization() {
        // Allowed chars pass through untouched.
        assert_eq!(sanitize_group("team-a"), "team-a");
        assert_eq!(sanitize_group("pool_1.beta"), "pool_1.beta");
        // Disallowed chars → '-'.
        assert_eq!(sanitize_group("my team!"), "my-team-");
        assert_eq!(sanitize_group("a/b"), "a-b");
        // Empty / dot names are not safe_group → Default.
        assert_eq!(sanitize_group(""), "Default");
        assert_eq!(sanitize_group("."), "Default");
        assert_eq!(sanitize_group(".."), "Default");
        // Truncated to 64 chars and still safe.
        let long = "x".repeat(100);
        let s = sanitize_group(&long);
        assert_eq!(s.len(), 64);
        assert!(crate::cliproxy::safe_group(&s));
        // A sanitized name is always safe_group-valid (or Default).
        assert!(crate::cliproxy::safe_group(&sanitize_group("weird🙂name")));
    }

    #[test]
    fn claude_auth_json_shape() {
        let acct = ClaudeAccount {
            email: "a@b.com".into(),
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            expires_at: 1_609_459_200_000, // 2021-01-01T00:00:00Z in ms
        };
        let v = claude_auth_json(&acct, "2026-07-20T00:00:00Z");
        assert_eq!(v["type"], "claude");
        assert_eq!(v["id_token"], "", "claude store has no id_token → empty string");
        assert_eq!(v["access_token"], "AT");
        assert_eq!(v["refresh_token"], "RT");
        assert_eq!(v["email"], "a@b.com");
        // expiresAt ms / 1000 → epoch seconds → RFC3339.
        assert_eq!(v["expired"], "2021-01-01T00:00:00Z");
        assert!(is_rfc3339(v["expired"].as_str().unwrap()));
        assert!(is_rfc3339(v["last_refresh"].as_str().unwrap()));
        // No codex-only field leaks in.
        assert!(v.get("account_id").is_none());
    }

    #[test]
    fn codex_auth_json_shape() {
        let acct = CodexAccount {
            email: "z@o.com".into(),
            account_id: "acc-1".into(),
            access_token: "AT2".into(),
            id_token: "IDT".into(),
            refresh_token: "RT2".into(),
            expires_at: 1_582_979_696_000, // 2020-02-29T12:34:56Z in ms
        };
        let v = codex_auth_json(&acct, "2026-07-20T00:00:00Z");
        assert_eq!(v["type"], "codex");
        assert_eq!(v["account_id"], "acc-1");
        assert_eq!(v["id_token"], "IDT", "codex carries a real id_token");
        assert_eq!(v["access_token"], "AT2");
        assert_eq!(v["refresh_token"], "RT2");
        assert_eq!(v["email"], "z@o.com");
        assert_eq!(v["expired"], "2020-02-29T12:34:56Z");
        assert!(is_rfc3339(v["expired"].as_str().unwrap()));
        assert!(is_rfc3339(v["last_refresh"].as_str().unwrap()));
    }

    #[test]
    fn write_auth_file_sets_perms_and_valid_json() {
        let dir = std::env::temp_dir().join(format!("token-migrate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let auth_dir = dir.join("cliproxy").join("team").join("auth");
        let body = claude_auth_json(
            &ClaudeAccount { email: "a@b.com".into(), access_token: "AT".into(), refresh_token: "RT".into(), expires_at: 0 },
            "2026-07-20T00:00:00Z",
        );
        let path = write_auth_file(&auth_dir, "claude-a@b.com.json", &body).unwrap();

        // Auth dir is 0700, file is 0600.
        let dir_mode = std::fs::metadata(&auth_dir).unwrap().permissions().mode() & 0o777;
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "auth-dir must be 0700");
        assert_eq!(file_mode, 0o600, "auth file must be 0600");

        // The bytes round-trip to the same JSON.
        let back: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back["type"], "claude");
        assert_eq!(back["access_token"], "AT");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_stores_deserialize_camelcase_and_ignore_extras() {
        // camelCase keys + ignored fields (orgUuid/active/id) must parse; expiresAt is ms.
        let store: ClaudeStore = serde_json::from_str(
            r#"{"accounts":[{"email":"a@b.com","accessToken":"AT","refreshToken":"RT","expiresAt":1609459200000,"orgUuid":"o","active":true,"id":"x"}]}"#,
        )
        .unwrap();
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(store.accounts[0].email, "a@b.com");
        assert_eq!(store.accounts[0].expires_at, 1_609_459_200_000);

        let store: CodexStore = serde_json::from_str(
            r#"{"accounts":[{"email":"z@o.com","accountId":"acc-1","accessToken":"AT2","idToken":"IDT","refreshToken":"RT2","expiresAt":123,"plan":"pro"}]}"#,
        )
        .unwrap();
        assert_eq!(store.accounts[0].account_id, "acc-1");
        assert_eq!(store.accounts[0].id_token, "IDT");

        // The old config's group lists parse from camelCase keys.
        let cfg: RawOldConfig = serde_json::from_str(
            r#"{"cloneGroups":[{"name":"team","accounts":["a@b.com"]}],"codexGroups":[{"name":"gpt","accounts":["z@o.com"]}],"somethingElse":42}"#,
        )
        .unwrap();
        assert_eq!(cfg.clone_groups[0].name, "team");
        assert_eq!(cfg.codex_groups[0].accounts, vec!["z@o.com".to_string()]);
    }
}
