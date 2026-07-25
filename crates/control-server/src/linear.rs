//! Linear integration for ticket-driven cloning — Rust port of `linear.server.ts`.
//! Talks to `api.linear.app/graphql` with a personal API key (`Authorization: <key>`,
//! no "Bearer"). Keys live on presets (`AppConfig.presets`), so a ticket is fetched by
//! trying each preset's key ([`fetch_issue_any`]); the ticket-id prefix then picks the
//! preset ([`pick_preset_by_prefix`]). The ticket-id prefix (e.g. `WE-142` → `we`)
//! names the team within whichever workspace the key can see.

use serde_json::{Value, json};

const LINEAR_API: &str = "https://api.linear.app/graphql";
const ISSUE_FIELDS: &str =
    "id identifier title url branchName state { id name type } labels { nodes { name } }";

#[derive(Debug)]
pub struct LinearError(pub String);
impl std::fmt::Display for LinearError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for LinearError {}

#[derive(Debug, Clone)]
pub struct TicketRef {
    /// Lowercase workspace name, e.g. `"we"`.
    pub prefix: String,
    pub team_key: String,
    pub number: u64,
    pub identifier: String,
}

#[derive(Debug, Clone)]
pub struct IssueInfo {
    /// Lowercase workspace name, e.g. `"we"`.
    pub prefix: String,
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub url: String,
    pub branch: String,
    pub state_type: String,
    /// All ticket labels, in Linear's order (used for preset auto-selection).
    pub labels: Vec<String>,
}

/// Pull a `WE-142`-style ref out of a pasted link or bare id (no regex crate).
pub fn parse_ticket_ref(input: &str) -> Result<TicketRef, LinearError> {
    let t = input.trim();
    let b = t.as_bytes();
    for start in 0..b.len() {
        if start > 0 && b[start - 1].is_ascii_alphanumeric() {
            continue; // not a word boundary
        }
        let mut i = start;
        while i < b.len() && b[i].is_ascii_alphabetic() {
            i += 1;
        }
        if i - start < 2 || i >= b.len() || b[i] != b'-' {
            continue;
        }
        let ds = i + 1;
        let mut j = ds;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j == ds || (j < b.len() && b[j].is_ascii_alphanumeric()) {
            continue;
        }
        let team_key = t[start..i].to_uppercase();
        let number: u64 = t[ds..j].parse().map_err(|_| LinearError("bad ticket number".into()))?;
        return Ok(TicketRef {
            prefix: team_key.to_ascii_lowercase(),
            team_key: team_key.clone(),
            number,
            identifier: format!("{team_key}-{number}"),
        });
    }
    Err(LinearError(format!("could not find a ticket id (like WE-142) in \"{input}\"")))
}

async fn gql(
    http: &reqwest::Client,
    key: &str,
    query: &str,
    variables: Value,
) -> Result<Value, LinearError> {
    if key.is_empty() {
        return Err(LinearError("no Linear API key configured for that workspace".into()));
    }
    let resp = http
        .post(LINEAR_API)
        .header("content-type", "application/json")
        .header("authorization", key)
        .json(&json!({ "query": query, "variables": variables }))
        .send()
        .await
        .map_err(|e| LinearError(format!("Linear API unreachable: {e}")))?;
    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| LinearError(format!("Linear API bad JSON: {e}")))?;
    if let Some(errs) = body.get("errors").and_then(Value::as_array) {
        if !errs.is_empty() {
            let msg = errs
                .iter()
                .filter_map(|e| e.get("message").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(LinearError(msg));
        }
    }
    if !status.is_success() {
        return Err(LinearError(format!("Linear API error (HTTP {})", status.as_u16())));
    }
    body.get("data").cloned().ok_or_else(|| LinearError("Linear API returned no data".into()))
}

fn to_issue_info(prefix: &str, n: &Value) -> IssueInfo {
    let s = |k: &str| n.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    IssueInfo {
        prefix: prefix.to_string(),
        id: s("id"),
        identifier: s("identifier"),
        title: s("title"),
        url: s("url"),
        branch: s("branchName"),
        state_type: n
            .get("state")
            .and_then(|st| st.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        labels: n
            .pointer("/labels/nodes")
            .and_then(Value::as_array)
            .map(|nodes| {
                nodes
                    .iter()
                    .filter_map(|l| l.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Fetch an existing issue by team key + number, with an explicit API key.
pub async fn fetch_issue(
    http: &reqwest::Client,
    key: &str,
    r: &TicketRef,
) -> Result<IssueInfo, LinearError> {
    let query = format!(
        "query($team: String!, $num: Float!) {{ issues(filter: {{ team: {{ key: {{ eq: $team }} }}, number: {{ eq: $num }} }}, first: 1) {{ nodes {{ {ISSUE_FIELDS} }} }} }}"
    );
    let data = gql(http, key, &query, json!({ "team": r.team_key, "num": r.number })).await?;
    let node = data.pointer("/issues/nodes/0").cloned().filter(|v| !v.is_null());
    match node {
        Some(n) => Ok(to_issue_info(&r.prefix, &n)),
        None => Err(LinearError(format!("ticket {} not found in Linear", r.identifier))),
    }
}

/// Fetch an issue by trying each of `keys` in order (dedup'd); the first success
/// wins. Returns the issue plus the key that fetched it — proven to have access, so
/// callers reuse it for follow-up mutations like [`ensure_in_progress`].
pub async fn fetch_issue_any(
    http: &reqwest::Client,
    keys: &[&str],
    r: &TicketRef,
) -> Result<(IssueInfo, String), LinearError> {
    let mut seen: Vec<&str> = Vec::new();
    let mut last_err: Option<LinearError> = None;
    for key in keys {
        if key.is_empty() || seen.contains(key) {
            continue;
        }
        seen.push(key);
        match fetch_issue(http, key, r).await {
            Ok(issue) => return Ok((issue, key.to_string())),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        LinearError("no preset has a Linear API key configured — add one in Settings".into())
    }))
}

/// The first preset (config order) with a label matching the ticket-id `prefix`
/// (case-insensitive), e.g. a preset labelled `DEV` matches `DEV-196` (prefix `dev`).
/// Presets with no labels never auto-match.
pub fn pick_preset_by_prefix<'a>(
    presets: &'a [wire::Preset],
    prefix: &str,
) -> Option<&'a wire::Preset> {
    presets.iter().find(|p| p.labels.iter().any(|pl| pl.eq_ignore_ascii_case(prefix)))
}

/// Re-host a `/uploads/<name>` image in Linear and return its permanent `assetUrl`.
///
/// The clone dialog's rich-text description uploads pasted images to the control-server's
/// own `/uploads` store (same path the clone notes use), which is LAN-only — a Linear
/// ticket referencing it would render a broken image for anyone outside the network. So
/// before the issue is created, every local image is re-uploaded through Linear's
/// `fileUpload` mutation: it hands back a pre-signed PUT plus the `assetUrl` that will
/// serve the file afterwards. The signed headers must be sent verbatim.
pub async fn upload_asset(
    http: &reqwest::Client,
    key: &str,
    filename: &str,
    content_type: &str,
    bytes: Vec<u8>,
) -> Result<String, LinearError> {
    let data = gql(
        http,
        key,
        "mutation($contentType: String!, $filename: String!, $size: Int!) { \
           fileUpload(contentType: $contentType, filename: $filename, size: $size) { \
             success uploadFile { uploadUrl assetUrl headers { key value } } } }",
        json!({ "contentType": content_type, "filename": filename, "size": bytes.len() }),
    )
    .await?;
    let f = data
        .pointer("/fileUpload/uploadFile")
        .filter(|v| !v.is_null())
        .ok_or_else(|| LinearError("Linear refused the file upload".into()))?;
    let upload_url = f
        .get("uploadUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| LinearError("Linear upload reply had no uploadUrl".into()))?;
    let asset_url = f
        .get("assetUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| LinearError("Linear upload reply had no assetUrl".into()))?
        .to_string();

    let mut req = http.put(upload_url).header("content-type", content_type);
    // Every header Linear signed into the URL has to go back verbatim or the PUT 403s.
    for h in f.get("headers").and_then(Value::as_array).into_iter().flatten() {
        if let (Some(k), Some(v)) = (
            h.get("key").and_then(Value::as_str),
            h.get("value").and_then(Value::as_str),
        ) {
            req = req.header(k, v);
        }
    }
    let resp = req
        .body(bytes)
        .send()
        .await
        .map_err(|e| LinearError(format!("Linear asset upload failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(LinearError(format!(
            "Linear asset upload failed (HTTP {})",
            resp.status().as_u16()
        )));
    }
    Ok(asset_url)
}

/// Every distinct `/uploads/<name>` file referenced by a markdown body, in first-seen order.
///
/// A generated upload name is `<hex>.<ext>` (see `files::save_upload`), so the name runs to
/// the first character that is neither alphanumeric nor `.` — which stops at the `)` closing
/// a markdown image, at whitespace, and at a query string. `read_upload` re-validates the
/// shape, so a crafted `../` can't escape the uploads dir even if it were scanned here.
fn upload_names(markdown: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut rest = markdown;
    while let Some(i) = rest.find("/uploads/") {
        let after = &rest[i + "/uploads/".len()..];
        let end = after
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '.')
            .unwrap_or(after.len());
        let name = &after[..end];
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
        rest = &after[end..];
    }
    names
}

/// Rewrite every `/uploads/<name>` reference in a markdown body to a Linear-hosted
/// `assetUrl`, uploading each distinct file once. A file that fails to upload keeps its
/// original URL — a ticket with one unreachable image beats no ticket at all — and the
/// error is logged, not returned.
pub async fn rehost_markdown_images(
    http: &reqwest::Client,
    key: &str,
    data_dir: &str,
    markdown: &str,
) -> String {
    let mut out = markdown.to_string();
    for name in upload_names(markdown) {
        let (bytes, ct) = match crate::files::read_upload(data_dir, &name) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("linear: image /uploads/{name} unreadable: {e} (left as-is)");
                continue;
            }
        };
        match upload_asset(http, key, &name, ct, bytes).await {
            Ok(url) => out = out.replace(&format!("/uploads/{name}"), &url),
            Err(e) => tracing::warn!("linear: re-hosting /uploads/{name} failed: {e} (left as-is)"),
        }
    }
    out
}

/// Create a new issue in team `prefix`, with an explicit API key.
pub async fn create_issue(
    http: &reqwest::Client,
    key: &str,
    prefix: &str,
    title: &str,
    description: &str,
) -> Result<IssueInfo, LinearError> {
    let tk = prefix.to_uppercase();
    // Resolve the team id AND the API key's owner in one round-trip: `viewer` is the Linear
    // user the key authenticates as (its owner), so we assign the new ticket to them below.
    let team_data = gql(
        http,
        key,
        "query($team: String!) { teams(filter: { key: { eq: $team } }, first: 1) { nodes { id } } viewer { id } }",
        json!({ "team": &tk }),
    )
    .await?;
    let team_id = team_data
        .pointer("/teams/nodes/0/id")
        .and_then(Value::as_str)
        .ok_or_else(|| LinearError(format!("team {tk} not found")))?
        .to_string();
    // The key owner's user id. Present for a personal API key; absent/null for an app/OAuth
    // actor with no personal user — in which case `assigneeId: null` leaves the ticket
    // unassigned rather than failing creation.
    let assignee_id = team_data.pointer("/viewer/id").and_then(Value::as_str);
    let mutation = format!(
        "mutation($teamId: String!, $title: String!, $description: String!, $assigneeId: String) {{ issueCreate(input: {{ teamId: $teamId, title: $title, description: $description, assigneeId: $assigneeId }}) {{ success issue {{ {ISSUE_FIELDS} }} }} }}"
    );
    let created = gql(
        http,
        key,
        &mutation,
        json!({ "teamId": team_id, "title": title, "description": description, "assigneeId": assignee_id }),
    )
    .await?;
    let ok = created.pointer("/issueCreate/success").and_then(Value::as_bool).unwrap_or(false);
    let node = created.pointer("/issueCreate/issue").cloned().filter(|v| !v.is_null());
    match (ok, node) {
        (true, Some(n)) => Ok(to_issue_info(prefix, &n)),
        _ => Err(LinearError(format!("failed to create ticket in {tk}"))),
    }
}


/// Move the issue to "In Progress" unless already started (no backwards drag).
/// `key` should be one proven to see the issue (e.g. the [`fetch_issue_any`] key).
pub async fn ensure_in_progress(
    http: &reqwest::Client,
    key: &str,
    issue: &IssueInfo,
) -> Result<(), LinearError> {
    if issue.state_type == "started" {
        return Ok(());
    }
    let tk = issue.prefix.to_uppercase();
    let data = gql(
        http,
        key,
        "query($team: String!) { teams(filter: { key: { eq: $team } }, first: 1) { nodes { states { nodes { id name type } } } } }",
        json!({ "team": &tk }),
    )
    .await?;
    let states = data
        .pointer("/teams/nodes/0/states/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let target = states
        .iter()
        .find(|s| s.get("name").and_then(Value::as_str) == Some("In Progress"))
        .or_else(|| states.iter().find(|s| s.get("type").and_then(Value::as_str) == Some("started")))
        .and_then(|s| s.get("id").and_then(Value::as_str))
        .ok_or_else(|| LinearError(format!("no \"In Progress\" state found for team {tk}")))?
        .to_string();
    let upd = gql(
        http,
        key,
        "mutation($id: String!, $state: String!) { issueUpdate(id: $id, input: { stateId: $state }) { success } }",
        json!({ "id": issue.id, "state": target }),
    )
    .await?;
    if upd.pointer("/issueUpdate/success").and_then(Value::as_bool) != Some(true) {
        return Err(LinearError(format!("failed to move {} to In Progress", issue.identifier)));
    }
    Ok(())
}

/// Sanitize the configurable hostname prefix to DNS-label-safe chars: lowercase,
/// keep `[a-z0-9-]`, drop a leading `-` (a trailing one like `pega-` is intended).
pub fn clean_prefix(prefix: &str) -> String {
    let s: String = prefix
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    s.trim_start_matches('-').to_string()
}

/// `(pega-, My cool task!)` → `pega-my-cool-task` (a DNS label; start_clone re-validates).
pub fn plain_hostname_base(prefix: &str, title: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in title.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let slug = slug.trim_matches('-').chars().take(40).collect::<String>();
    let slug = slug.trim_matches('-').to_string();
    let prefix = clean_prefix(prefix);
    if slug.is_empty() { format!("{prefix}host") } else { format!("{prefix}{slug}") }
}

/// `(pega-, DEV-123)` → `pega-dev-123`.
pub fn ticket_hostname_base(prefix: &str, identifier: &str) -> String {
    format!("{}{}", clean_prefix(prefix), identifier.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_names_finds_each_distinct_image_once() {
        let md = "![a](/uploads/ab12.png)\n\ntext ![b](/uploads/cd34.jpeg) and ![a again](/uploads/ab12.png)";
        assert_eq!(upload_names(md), vec!["ab12.png", "cd34.jpeg"]);
        // A body with no images does no work at all.
        assert!(upload_names("# just a heading\n\nno images here").is_empty());
        // The name stops at the first non-name character, so an absolute URL form and a
        // query string both yield the bare file name the uploads store is keyed by.
        assert_eq!(
            upload_names("http://10.0.0.84:9000/uploads/ff99.png?v=2"),
            vec!["ff99.png"]
        );
        // A bare `/uploads/` with nothing after it isn't a reference.
        assert!(upload_names("see /uploads/ for details").is_empty());
    }

    #[test]
    fn parses_ticket_refs() {
        let r = parse_ticket_ref("https://linear.app/x/issue/WE-142/foo").unwrap();
        assert_eq!(r.identifier, "WE-142");
        assert_eq!(r.prefix, "we");
        assert_eq!(parse_ticket_ref("dev-7").unwrap().identifier, "DEV-7");
        // Any prefix parses now (workspaces are config, not an enum); whether a key
        // exists for it is checked at fetch/create time.
        assert_eq!(parse_ticket_ref("XX-1").unwrap().prefix, "xx");
        assert!(parse_ticket_ref("nope").is_err());
    }

    #[test]
    fn picks_preset_by_ticket_prefix() {
        let p = |name: &str, labels: &[&str]| wire::Preset {
            name: name.into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let presets = [p("front", &["WE", "UI"]), p("back", &["DEV"]), p("nolabel", &[])];
        // Case-insensitive match against the (lowercase) ticket-id prefix.
        assert_eq!(pick_preset_by_prefix(&presets, "dev").unwrap().name, "back");
        // Multiple labels on a preset → any of them can match.
        assert_eq!(pick_preset_by_prefix(&presets, "we").unwrap().name, "front");
        // No matching prefix / labelless presets never auto-match.
        assert!(pick_preset_by_prefix(&presets, "docs").is_none());
        assert!(pick_preset_by_prefix(&presets, "").is_none());
    }

    #[test]
    fn plain_slug() {
        assert_eq!(plain_hostname_base("pega-", "My cool task!"), "pega-my-cool-task");
        assert_eq!(plain_hostname_base("pega-", "!!!"), "pega-host");
        // custom + sanitized prefixes
        assert_eq!(plain_hostname_base("clone-", "My task"), "clone-my-task");
        assert_eq!(plain_hostname_base("", "My task"), "my-task");
        assert_eq!(plain_hostname_base("-Bad_Pre-", "X"), "badpre-x"); // leading '-' dropped, '_' stripped, lowercased
    }

    #[test]
    fn ticket_base() {
        assert_eq!(ticket_hostname_base("pega-", "DEV-123"), "pega-dev-123");
        assert_eq!(ticket_hostname_base("", "WE-7"), "we-7");
    }
}
