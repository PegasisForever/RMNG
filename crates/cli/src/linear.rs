//! The Linear calls `rmng clone create ticket` and `create new-ticket` make for themselves.
//!
//! The control-server has no Linear route: the dashboard's browser holds the preset keys
//! (`GET /api/config` hands them out) and talks to `api.linear.app` directly, then posts the
//! answer with the fork request. The CLI does the same, so both clients follow one set of
//! rules, which are the browser's (`frontend/app/lib/linear/{issues,mutations,people}.ts`):
//!
//! - An issue is looked up by team key AND number (Linear's filter has no identifier field),
//!   with each preset key in turn, the team's own key first. The key that sees it writes it.
//! - A new issue goes into the team's first Todo state (lowest `position` of type
//!   `unstarted`), and to the key's owner unless someone else is named.
//! - "In Progress" means a state with that name, else the first `started` one. An issue that
//!   is already started is not moved, so a second clone of one ticket does not drag it back.
//!
//! Auth is the personal API key as-is, with no `Bearer`.

use std::time::Duration;

use serde_json::{Value, json};

const LINEAR_API: &str = "https://api.linear.app/graphql";

/// What a clone needs from an issue, plus the state the move decision reads.
const ISSUE_FIELDS: &str =
    "id identifier title url branchName state { id name type } labels { nodes { name } }";

pub type Result<T> = std::result::Result<T, String>;

/// A `WE-142` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketRef {
    /// Lowercase team key, `we`. What picks the preset.
    pub prefix: String,
    /// Uppercase team key, `WE`, the way Linear stores it.
    pub team: String,
    pub number: u64,
    /// `WE-142`.
    pub identifier: String,
}

/// One issue, as the clone request and the move to In Progress need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Lowercase team key.
    pub prefix: String,
    /// Linear's UUID.
    pub uuid: String,
    pub identifier: String,
    pub title: String,
    pub url: String,
    pub branch: String,
    /// Linear's state type, verbatim (`unstarted`, `started`, `completed`, …).
    pub state_type: String,
    pub labels: Vec<String>,
}

/// The first `WE-142` in a Linear link or a bare id: two or more letters, a dash, digits,
/// on word boundaries. The same rule as the browser's `parseTicketInput`.
pub fn parse_ticket_ref(input: &str) -> Result<TicketRef> {
    let b = input.as_bytes();
    let word = |i: usize| {
        b.get(i)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
    };
    for start in 0..b.len() {
        if start > 0 && word(start - 1) {
            continue;
        }
        let mut i = start;
        while i < b.len() && b[i].is_ascii_alphabetic() {
            i += 1;
        }
        if i - start < 2 || b.get(i) != Some(&b'-') {
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j == i + 1 || word(j) {
            continue;
        }
        let Ok(number) = input[i + 1..j].parse() else {
            continue;
        };
        let team = input[start..i].to_ascii_uppercase();
        return Ok(TicketRef {
            prefix: team.to_ascii_lowercase(),
            identifier: format!("{team}-{number}"),
            team,
            number,
        });
    }
    Err(format!(
        "could not find a ticket id (like WE-142) in \"{input}\""
    ))
}

/// The first preset whose label is `team`, case-insensitive. Config order, the same rule as
/// the server's and the browser's.
pub fn preset_for_team<'a>(
    presets: &'a [wire::PresetRedacted],
    team: &str,
) -> Option<&'a wire::PresetRedacted> {
    let team = team.trim();
    presets
        .iter()
        .find(|p| p.labels.iter().any(|l| l.trim().eq_ignore_ascii_case(team)))
}

/// Every team key a preset names, lowercase, in config order.
pub fn teams(presets: &[wire::PresetRedacted]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in presets {
        for l in &p.labels {
            let k = l.trim().to_ascii_lowercase();
            if !k.is_empty() && !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out
}

/// Every distinct key that could own `team`: the keys of presets labelled with it first,
/// then the rest. A blank team gives every key in config order.
pub fn keys_for_team(presets: &[wire::PresetRedacted], team: &str) -> Vec<String> {
    let wanted = team.trim();
    let claims = |p: &wire::PresetRedacted| {
        !wanted.is_empty()
            && p.labels
                .iter()
                .any(|l| l.trim().eq_ignore_ascii_case(wanted))
    };
    let mut keys: Vec<String> = Vec::new();
    for only in [true, false] {
        for p in presets.iter().filter(|p| claims(p) == only) {
            let k = p.linear_key.trim();
            if !k.is_empty() && !keys.iter().any(|x| x == k) {
                keys.push(k.to_string());
            }
        }
    }
    keys
}

/// The issue as a clone request carries it: every field is Linear's own.
pub fn clone_meta(issue: &Issue) -> wire::LinearMeta {
    wire::LinearMeta {
        workspace: Some(issue.prefix.clone()),
        ticket: Some(issue.identifier.clone()),
        ticket_url: Some(issue.url.clone()),
        branch: Some(issue.branch.clone()),
        display_name: Some(issue.title.clone()),
        label: issue.labels.first().filter(|l| !l.is_empty()).cloned(),
    }
}

pub struct Linear {
    http: reqwest::Client,
}

impl Linear {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Linear { http }
    }

    /// One GraphQL call. A 200 can still be a refusal: the `errors` array decides.
    async fn gql(&self, key: &str, query: &str, variables: Value) -> Result<Value> {
        if key.trim().is_empty() {
            return Err("no Linear API key configured for that team (Settings → Presets)".into());
        }
        let resp = self
            .http
            .post(LINEAR_API)
            .header("authorization", key.trim())
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .map_err(|e| format!("Linear API unreachable: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if let Some(msg) = body["errors"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e["message"].as_str())
            .find(|m| !m.trim().is_empty())
        {
            return Err(format!("Linear: {msg}"));
        }
        if !status.is_success() {
            return Err(format!("Linear API error (HTTP {})", status.as_u16()));
        }
        match body.get("data") {
            Some(d) if !d.is_null() => Ok(d.clone()),
            _ => Err("Linear API returned no data".into()),
        }
    }

    async fn fetch(&self, key: &str, r: &TicketRef) -> Result<Issue> {
        let query = format!(
            "query($team: String!, $num: Float!) {{ issues(filter: {{ team: {{ key: {{ eq: $team }} }}, \
             number: {{ eq: $num }} }}, first: 1) {{ nodes {{ {ISSUE_FIELDS} }} }} }}"
        );
        let data = self
            .gql(key, &query, json!({ "team": r.team, "num": r.number }))
            .await?;
        issue_from_node(&r.prefix, &data["issues"]["nodes"][0])
            .ok_or_else(|| format!("ticket {} not found in Linear", r.identifier))
    }

    /// The issue `r` names, with the first key that sees it. That key comes back too:
    /// it is the one proven to be allowed to write the issue.
    pub async fn find(&self, keys: &[String], r: &TicketRef) -> Result<(Issue, String)> {
        let mut last = None;
        for key in keys {
            match self.fetch(key, r).await {
                Ok(issue) => return Ok((issue, key.clone())),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| {
            "no preset has a Linear API key (add one in Settings → Presets)".into()
        }))
    }

    /// The team's members who can hold a ticket, and the key owner's own id.
    pub async fn people(&self, key: &str, team: &str) -> Result<(Vec<Person>, String)> {
        let data = self
            .gql(
                key,
                "query($team: String!) { teams(filter: { key: { eq: $team } }, first: 1) { \
                 nodes { members(first: 100) { nodes { id name displayName email active } } } } \
                 viewer { id } }",
                json!({ "team": team.to_ascii_uppercase() }),
            )
            .await?;
        let viewer = data["viewer"]["id"].as_str().unwrap_or("").to_string();
        let people = data["teams"]["nodes"][0]["members"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|n| n["active"] != Value::Bool(false))
            .filter_map(|n| {
                let s = |k: &str| n[k].as_str().unwrap_or("").to_string();
                let id = s("id");
                let email = s("email");
                let name = [s("name"), s("displayName"), email.clone()]
                    .into_iter()
                    .find(|x| !x.is_empty())?;
                (!id.is_empty()).then_some(Person { id, name, email })
            })
            .collect();
        Ok((people, viewer))
    }

    /// Open an issue in `team`'s first Todo state.
    pub async fn create(&self, key: &str, new: &NewIssue) -> Result<Issue> {
        let team = new.team.trim().to_ascii_uppercase();
        let data = self
            .gql(
                key,
                "query($team: String!) { teams(filter: { key: { eq: $team } }, first: 1) { \
                 nodes { id states { nodes { id type position } } } } viewer { id } }",
                json!({ "team": team }),
            )
            .await?;
        let node = &data["teams"]["nodes"][0];
        let team_id = node["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("team {team} not found in Linear"))?;
        let mut input = json!({
            "teamId": team_id,
            "title": new.title,
            "description": new.description,
        });
        let assignee = new
            .assignee_id
            .clone()
            .or_else(|| data["viewer"]["id"].as_str().map(str::to_string))
            .filter(|s| !s.is_empty());
        if let Some(a) = assignee {
            input["assigneeId"] = json!(a);
        }
        if let Some(state) = pick_state(&node["states"]["nodes"], "unstarted") {
            input["stateId"] = json!(state);
        }
        if let Some(p) = new.priority.filter(|p| *p > 0) {
            input["priority"] = json!(p);
        }
        let query = format!(
            "mutation($input: IssueCreateInput!) {{ issueCreate(input: $input) {{ success issue {{ {ISSUE_FIELDS} }} }} }}"
        );
        let created = self.gql(key, &query, json!({ "input": input })).await?;
        if created["issueCreate"]["success"] != Value::Bool(true) {
            return Err(format!("Linear refused a new ticket in {team}"));
        }
        issue_from_node(&team.to_ascii_lowercase(), &created["issueCreate"]["issue"])
            .ok_or_else(|| format!("Linear refused a new ticket in {team}"))
    }

    /// Move the issue to In Progress unless it is already started. Answers whether it moved.
    pub async fn start(&self, key: &str, issue: &Issue) -> Result<bool> {
        if issue.state_type == "started" {
            return Ok(false);
        }
        let team = issue.prefix.to_ascii_uppercase();
        let data = self
            .gql(
                key,
                "query($team: String!) { teams(filter: { key: { eq: $team } }, first: 1) { \
                 nodes { states { nodes { id name type } } } } }",
                json!({ "team": team }),
            )
            .await?;
        let state = pick_in_progress(&data["teams"]["nodes"][0]["states"]["nodes"])
            .ok_or_else(|| format!("no \"In Progress\" state found for team {team}"))?;
        let upd = self
            .gql(
                key,
                "mutation($id: String!, $state: String!) { issueUpdate(id: $id, input: { stateId: $state }) { success } }",
                json!({ "id": issue.uuid, "state": state }),
            )
            .await?;
        if upd["issueUpdate"]["success"] != Value::Bool(true) {
            return Err(format!(
                "failed to move {} to In Progress",
                issue.identifier
            ));
        }
        Ok(true)
    }
}

/// A ticket `create` opens.
pub struct NewIssue {
    pub team: String,
    pub title: String,
    pub description: String,
    /// 1 urgent through 4 low; `None` is no priority.
    pub priority: Option<u8>,
    /// Linear user UUID. `None` gives it to the key's owner.
    pub assignee_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub email: String,
}

/// The team member `who` names: `me` (the key's owner), an email, or a name, all
/// case-insensitive. An error lists who there is.
pub fn pick_person<'a>(people: &'a [Person], viewer: &str, who: &str) -> Result<&'a Person> {
    let w = who.trim();
    let found = if w.eq_ignore_ascii_case("me") {
        people.iter().find(|p| p.id == viewer)
    } else {
        people
            .iter()
            .find(|p| p.email.eq_ignore_ascii_case(w))
            .or_else(|| people.iter().find(|p| p.name.eq_ignore_ascii_case(w)))
    };
    found.ok_or_else(|| {
        let names: Vec<String> = people
            .iter()
            .map(|p| format!("{} <{}>", p.name, p.email))
            .collect();
        format!("no team member '{w}' (have: {})", names.join(", "))
    })
}

fn issue_from_node(prefix: &str, n: &Value) -> Option<Issue> {
    let s = |k: &str| n[k].as_str().unwrap_or("").to_string();
    let identifier = s("identifier");
    if identifier.is_empty() {
        return None;
    }
    Some(Issue {
        prefix: prefix.to_string(),
        uuid: s("id"),
        identifier,
        title: s("title"),
        url: s("url"),
        branch: s("branchName"),
        state_type: n["state"]["type"].as_str().unwrap_or("").to_string(),
        labels: n["labels"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str())
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
    })
}

/// The lowest-`position` state of `ty`; a state with no position ranks last.
fn pick_state(nodes: &Value, ty: &str) -> Option<String> {
    nodes
        .as_array()?
        .iter()
        .filter(|n| n["type"] == ty)
        .min_by(|a, b| {
            let r = |n: &Value| n["position"].as_f64().unwrap_or(f64::MAX);
            r(a).total_cmp(&r(b))
        })
        .and_then(|n| n["id"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A state named "In Progress", else the first `started` one.
fn pick_in_progress(nodes: &Value) -> Option<String> {
    let all = nodes.as_array()?;
    all.iter()
        .find(|n| n["name"] == "In Progress")
        .or_else(|| all.iter().find(|n| n["type"] == "started"))
        .and_then(|n| n["id"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str, labels: &[&str], key: &str) -> wire::PresetRedacted {
        wire::PresetRedacted {
            name: name.into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            linear_key: key.into(),
            group: String::new(),
            default_fork_clone: String::new(),
            vars: Vec::new(),
            agent_playbook: String::new(),
            global_prompt: String::new(),
            startup_script: String::new(),
            dockerfile: String::new(),
        }
    }

    #[test]
    fn parses_ticket_refs_like_the_browser() {
        let r = parse_ticket_ref("https://linear.app/x/issue/WE-142/fix-it").unwrap();
        assert_eq!(
            (
                r.identifier.as_str(),
                r.prefix.as_str(),
                r.team.as_str(),
                r.number
            ),
            ("WE-142", "we", "WE", 142)
        );
        assert_eq!(parse_ticket_ref("dev-7").unwrap().identifier, "DEV-7");
        assert_eq!(parse_ticket_ref("  ng-12 ").unwrap().identifier, "NG-12");
        // Not on a word boundary, one letter, no digits: no match.
        assert!(parse_ticket_ref("xWE-1x").is_err());
        assert!(parse_ticket_ref("W-1").is_err());
        assert!(parse_ticket_ref("WE-").is_err());
        assert!(parse_ticket_ref("nope").is_err());
    }

    #[test]
    fn the_teams_own_key_comes_first() {
        let presets = [
            preset("a", &["DEV"], "k1"),
            preset("b", &["WE"], "k2"),
            preset("c", &["we", "ui"], "k2"),
            preset("d", &[], "k3"),
        ];
        assert_eq!(keys_for_team(&presets, "we"), ["k2", "k1", "k3"]);
        assert_eq!(keys_for_team(&presets, ""), ["k1", "k2", "k3"]);
        assert_eq!(preset_for_team(&presets, "We").unwrap().name, "b");
        assert!(preset_for_team(&presets, "docs").is_none());
        assert_eq!(teams(&presets), ["dev", "we", "ui"]);
    }

    #[test]
    fn picks_states_like_the_browser() {
        let nodes = json!([
            { "id": "s1", "name": "Backlog", "type": "backlog", "position": 0 },
            { "id": "s3", "name": "Ready", "type": "unstarted", "position": 2 },
            { "id": "s2", "name": "Todo", "type": "unstarted", "position": 1 },
            { "id": "s4", "name": "In Review", "type": "started", "position": 3 },
            { "id": "s5", "name": "In Progress", "type": "started", "position": 4 }
        ]);
        assert_eq!(pick_state(&nodes, "unstarted").as_deref(), Some("s2"));
        // The name wins over Linear's order among started states.
        assert_eq!(pick_in_progress(&nodes).as_deref(), Some("s5"));
        let no_name = json!([{ "id": "s4", "name": "Doing", "type": "started" }]);
        assert_eq!(pick_in_progress(&no_name).as_deref(), Some("s4"));
        assert_eq!(pick_in_progress(&json!([])), None);
    }

    #[test]
    fn picks_a_person_by_email_name_or_me() {
        let people = vec![
            Person {
                id: "u1".into(),
                name: "Ada Park".into(),
                email: "ada@x.com".into(),
            },
            Person {
                id: "u2".into(),
                name: "Bo".into(),
                email: "bo@x.com".into(),
            },
        ];
        assert_eq!(pick_person(&people, "u2", "me").unwrap().id, "u2");
        assert_eq!(pick_person(&people, "u2", "ADA@x.com").unwrap().id, "u1");
        assert_eq!(pick_person(&people, "u2", "ada park").unwrap().id, "u1");
        let err = pick_person(&people, "u2", "cy").unwrap_err();
        assert!(err.contains("Ada Park <ada@x.com>"), "{err}");
    }

    #[test]
    fn clone_meta_carries_linears_own_fields() {
        let issue = issue_from_node(
            "we",
            &json!({
                "id": "uuid", "identifier": "WE-142", "title": "Fix it",
                "url": "https://linear.app/x/issue/WE-142", "branchName": "we-142-fix-it",
                "state": { "type": "unstarted" }, "labels": { "nodes": [{ "name": "Bug" }, { "name": "UI" }] }
            }),
        )
        .unwrap();
        let m = clone_meta(&issue);
        assert_eq!(m.ticket.as_deref(), Some("WE-142"));
        assert_eq!(m.workspace.as_deref(), Some("we"));
        assert_eq!(m.display_name.as_deref(), Some("Fix it"));
        assert_eq!(m.branch.as_deref(), Some("we-142-fix-it"));
        assert_eq!(m.label.as_deref(), Some("Bug"));
        assert!(issue_from_node("we", &json!({ "title": "no id" })).is_none());
    }
}
