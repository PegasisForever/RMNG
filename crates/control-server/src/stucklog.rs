//! A durable record of every activity verdict, so a wrong one can be scored after the fact.
//!
//! **What was missing.** [`crate::stuck`] recomputes each verdict from scratch every tick and
//! keeps nothing but its answer cache. The transcript ledger says what a session went on to do,
//! and `state.json` says what the fleet looks like now, but neither says what the server
//! believed at the moment it decided. Without that half a verdict cannot be called wrong: you
//! can see a session came back to life and still not know whether anything had reported it
//! finished, what evidence that reading rested on, or whether a model was involved at all.
//!
//! **What this writes.** One NDJSON line per decision under `<data_dir>/stuck/<date>.ndjson`.
//! Every line repeats the clone and the session, so a grep hit reads on its own. A line is
//! written only when the decision is news: the session's state changed, the session went away,
//! or the model was actually called for it. A session sitting in one state costs nothing, which
//! is what keeps a 4-second tick over a whole fleet down to a few hundred lines a day.
//!
//! **How to score a verdict.** Find the line where a session went `idle`, then the next line for
//! that same session. If it says `working`, the session resumed. `promptAgeSeconds` on that
//! second line is how long the session had gone without a human prompt at the instant it came
//! back. When that age is larger than the gap between the two lines, nobody typed anything in
//! between and the earlier `idle` was wrong. `view` on the same line is the exact question the
//! model was asked, so a wrong answer can be replayed against a changed prompt.
//!
//! **Two clocks, and they are never mixed.** `ts` is this server's. Every `*Seconds` field is the
//! clone's own, because it is derived from the clone's event log, and [`crate::stuck`] differences
//! those only against each other. Subtracting one clock from the other is meaningless here, which
//! is why the comparison above is stated as a gap against an age rather than as two instants.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex as StdMutex, RwLock as StdRwLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How many daily files to keep. A month covers any investigation that starts from "this clone
/// has been behaving oddly lately" and still bounds the directory on a server that never stops.
const KEEP_DAYS: usize = 30;

/// What one billed model call cost, exactly as the provider reported it.
///
/// Kept as the provider's own counts rather than blended into a total: input and output bill at
/// different rates, and a sum cannot be taken apart again afterwards. Every field is optional
/// or defaulted because this is transport metadata, not an answer: a backend that stops sending
/// a usage record, or starts sending a richer one, must not turn a good verdict into an error.
///
/// Absent on every line no live call produced, which is what makes its presence the honest
/// answer to "was this one billed" — see [`Decision::asked`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// The share of `input_tokens` the provider served from its own prompt cache, when it says
    /// so. The judge re-sends a byte-identical 1,538-token prompt on every call, so this is the
    /// field that would show whether that prefix is discounted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    /// Hidden reasoning tokens, which bill as output. Absent unless the provider breaks them
    /// out, and it is the largest unknown in any cost estimate that does not have them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u64>,
}

/// One decision about one session, at the moment it was made.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    /// This SERVER's clock, RFC3339 UTC. Stamped by [`Recorder::record`], so a caller building
    /// one leaves it empty.
    pub ts: String,
    pub clone: String,
    /// The Claude Code session id or Cursor conversation id, in full. The `stuck` tracing lines
    /// carry the first eight characters, and this is what they abbreviate.
    pub session: String,
    /// `working`, `idle`, or `gone`.
    pub state: String,
    /// The state this line replaced, or `new` the first time a session is seen. Present so one
    /// line says what changed without its predecessor in hand.
    pub was: String,
    /// Where the answer came from: `files` when the registry settled it, `model` when GPT was
    /// called for this exact view, `cache` when a matching view had already been answered,
    /// `no-judge` when nothing could answer, `ask-failed` when the call errored.
    pub decided_by: String,
    /// The file reason, or the model's own sentence.
    pub why: String,
    /// The registry status the verdict was read from: `busy`, `shell`, `idle`, `waiting`, or
    /// absent for an entrypoint that publishes none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// What a `waiting` session wants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
    /// The CLONE's clock: seconds since this session's newest `UserPromptSubmit`. Absent when
    /// the hook log was not read, which is every session the registry settled on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_age_seconds: Option<f64>,
    /// Exactly what the model was asked, when it was asked anything. Absent on a file-settled
    /// session, which has no view: its whole evidence is `status` and `waitingFor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<Value>,
    /// Whether a model call was made AND answered for this line. Stamped by [`Recorder::record`]
    /// from the same flag that decides whether the line is written at all, so the two can never
    /// disagree.
    ///
    /// A call that was attempted and failed reads `false`, because the caller passes `false` on
    /// that path: those lines are `decidedBy: "ask-failed"` and are counted from that instead.
    /// It is deliberate — forcing them true would defeat the change test below and append a
    /// view-carrying line per session per tick for as long as a provider outage lasted.
    ///
    /// `decided_by` cannot answer this on its own and never could: the tool-age floor relabels
    /// an answered call `floor` (see `crate::stuck::overruled`), which leaves it
    /// indistinguishable from a free `cache` hit. Counting billed calls off the label alone
    /// therefore gives a bracket rather than a number, and this closes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub asked: bool,
    /// What the call cost, when the provider reported it. Absent on every file-settled or
    /// cached line, and on a billed call whose stream ended without a usage record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<CallUsage>,
}

/// What each session was last reported as, plus the lines this pass has yet to write.
///
/// Held on [`crate::app::App`] and shared by every clone in a fleet pass, so the change test is
/// against what was actually written rather than against anything recomputed.
#[derive(Default)]
pub struct Recorder {
    /// `(clone, session)` -> the state written for it last.
    seen: StdRwLock<HashMap<(String, String), String>>,
    /// Buffered rather than written per decision: a pass produces its lines across dozens of
    /// concurrent tasks, and one append at the end of it beats one per clone.
    pending: StdMutex<Vec<Decision>>,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue `decision` if it is news, filling in its `ts` and its `was`.
    ///
    /// `asked` forces the line even when the state held, because a live model call is worth
    /// keeping whatever it concluded: it is the only record of a question the cache had not
    /// already answered, and the prompt is tuned against exactly those.
    pub fn record(&self, mut decision: Decision, asked: bool) {
        let key = (decision.clone.clone(), decision.session.clone());
        let was = self.seen.read().unwrap().get(&key).cloned();
        if was.as_deref() == Some(decision.state.as_str()) && !asked {
            return;
        }
        decision.ts = now_rfc3339();
        decision.was = was.unwrap_or_else(|| "new".to_string());
        decision.asked = asked;
        self.seen
            .write()
            .unwrap()
            .insert(key, decision.state.clone());
        self.pending.lock().unwrap().push(decision);
    }

    /// Close out every session of `clone` that is no longer running, given the ones that are.
    ///
    /// A session that ends and a session that stops needing a person are different events, and
    /// only this tells them apart: without it the last thing a finished session ever said is
    /// `idle`, which reads as an agent still sitting at a prompt that no longer exists.
    ///
    /// Call it only for a clone that was actually read. A clone whose home is momentarily
    /// unreachable has no live set, and treating that as an empty one retires the whole clone.
    pub fn retire(&self, clone: &str, live: &[String]) {
        let stale: Vec<(String, String)> = self
            .seen
            .read()
            .unwrap()
            .keys()
            .filter(|(c, s)| c == clone && !live.iter().any(|l| l == s))
            .cloned()
            .collect();
        if stale.is_empty() {
            return;
        }
        let ts = now_rfc3339();
        let mut seen = self.seen.write().unwrap();
        let mut pending = self.pending.lock().unwrap();
        for key in stale {
            let was = seen.remove(&key).unwrap_or_default();
            pending.push(Decision {
                ts: ts.clone(),
                clone: key.0,
                session: key.1,
                state: "gone".to_string(),
                was,
                decided_by: "files".to_string(),
                why: "the session is no longer running".to_string(),
                ..Default::default()
            });
        }
    }

    /// Append everything queued, one open per day file. Blocking IO: run it off the async path.
    ///
    /// A failure here is logged and dropped. This is a debugging record, and losing a line is
    /// never a reason to disturb a fleet pass that has already decided correctly.
    pub fn flush(&self, data_dir: &str) {
        let queued: Vec<Decision> = std::mem::take(&mut *self.pending.lock().unwrap());
        if queued.is_empty() {
            return;
        }
        let dir = Path::new(data_dir).join("stuck");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(target: "stuck", "cannot create {}: {e}", dir.display());
            return;
        }
        for (day, body) in group_by_day(&queued) {
            let path = dir.join(format!("{day}.ndjson"));
            let opened = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path);
            match opened {
                Ok(mut file) => {
                    if let Err(e) = file.write_all(body.as_bytes()) {
                        tracing::warn!(target: "stuck", "cannot write {}: {e}", path.display());
                    }
                }
                Err(e) => tracing::warn!(target: "stuck", "cannot open {}: {e}", path.display()),
            }
        }
        prune(&dir);
    }

    /// Drop remembered sessions for clones no longer in the fleet, mirroring
    /// [`crate::monitor::ActivityBus::retain`]. A deleted clone leaves no `gone` lines: its
    /// sessions ended with it, and the delete is already recorded as an operation.
    pub fn retain(&self, clones: &std::collections::HashSet<String>) {
        self.seen
            .write()
            .unwrap()
            .retain(|(c, _), _| clones.contains(c));
    }
}

/// One NDJSON body per calendar day, keyed by the `YYYY-MM-DD` its lines carry.
///
/// A pass can straddle midnight, so grouping is by the line's own stamp rather than by one
/// filename chosen for the batch.
fn group_by_day(queued: &[Decision]) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for decision in queued {
        let Ok(line) = serde_json::to_string(decision) else {
            continue;
        };
        let day = decision.ts.get(..10).unwrap_or("undated").to_string();
        let body = out.entry(day).or_default();
        body.push_str(&line);
        body.push('\n');
    }
    out
}

/// Keep the newest [`KEEP_DAYS`] files and delete the rest.
///
/// Names are `YYYY-MM-DD.ndjson`, which sort by date lexically, so retention needs no clock and
/// cannot be confused by a file whose mtime moved.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut days: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ndjson"))
        .collect();
    if days.len() <= KEEP_DAYS {
        return;
    }
    days.sort();
    for path in &days[..days.len() - KEEP_DAYS] {
        let _ = std::fs::remove_file(path);
    }
}

fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    crate::docker::epoch_to_rfc3339(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(clone: &str, session: &str, state: &str) -> Decision {
        Decision {
            clone: clone.to_string(),
            session: session.to_string(),
            state: state.to_string(),
            decided_by: "files".to_string(),
            why: "idle at its prompt".to_string(),
            ..Default::default()
        }
    }

    fn drain(rec: &Recorder) -> Vec<Decision> {
        std::mem::take(&mut *rec.pending.lock().unwrap())
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rmng-stucklog-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_state_that_holds_is_written_once() {
        // The whole reason a 4-second tick over a fleet is affordable: an unchanged verdict
        // costs nothing after the first line.
        let rec = Recorder::new();
        for _ in 0..5 {
            rec.record(decision("c1", "s1", "idle"), false);
        }
        let lines = drain(&rec);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].was, "new");
        assert!(!lines[0].ts.is_empty(), "the recorder stamps the line");
    }

    #[test]
    fn a_changed_state_says_what_it_replaced() {
        let rec = Recorder::new();
        rec.record(decision("c1", "s1", "idle"), false);
        rec.record(decision("c1", "s1", "working"), false);
        let lines = drain(&rec);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            (lines[1].was.as_str(), lines[1].state.as_str()),
            ("idle", "working")
        );
    }

    #[test]
    fn a_live_model_call_is_kept_even_when_the_state_held() {
        // The cache answers most ticks, so the lines where a question was actually put to the
        // model are the ones worth having when the prompt is being tuned.
        let rec = Recorder::new();
        rec.record(decision("c1", "s1", "idle"), false);
        rec.record(decision("c1", "s1", "idle"), true);
        assert_eq!(drain(&rec).len(), 2);
    }

    #[test]
    fn two_sessions_in_one_clone_are_tracked_apart() {
        let rec = Recorder::new();
        rec.record(decision("c1", "s1", "idle"), false);
        rec.record(decision("c1", "s2", "idle"), false);
        assert_eq!(
            drain(&rec).len(),
            2,
            "s2 is news even though s1 said the same thing"
        );
    }

    #[test]
    fn a_vanished_session_is_closed_out_and_forgotten() {
        let rec = Recorder::new();
        rec.record(decision("c1", "s1", "idle"), false);
        rec.record(decision("c1", "s2", "working"), false);
        drain(&rec);

        rec.retire("c1", &["s2".to_string()]);
        let lines = drain(&rec);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            (lines[0].session.as_str(), lines[0].state.as_str()),
            ("s1", "gone")
        );
        assert_eq!(lines[0].was, "idle");

        // Forgotten, so the same session id coming back reads as new rather than as a change.
        rec.record(decision("c1", "s1", "idle"), false);
        assert_eq!(drain(&rec)[0].was, "new");
    }

    #[test]
    fn retiring_one_clone_leaves_another_alone() {
        let rec = Recorder::new();
        rec.record(decision("c1", "s1", "idle"), false);
        rec.record(decision("c2", "s1", "idle"), false);
        drain(&rec);
        rec.retire("c1", &[]);
        let lines = drain(&rec);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].clone, "c1");
    }

    #[test]
    fn the_scoring_fields_survive_a_round_trip() {
        // What an investigation actually reads: the evidence, the question, and the age of the
        // last human prompt. A line that loses any of them cannot settle whether a verdict was
        // wrong.
        let mut d = decision("c1", "s1", "working");
        d.decided_by = "model".to_string();
        d.prompt_age_seconds = Some(2712.0);
        d.view = Some(serde_json::json!({"session": {"status": "busy"}}));
        let json = serde_json::to_string(&d).unwrap();
        let back: Decision = serde_json::from_str(&json).unwrap();
        assert_eq!(back.prompt_age_seconds, Some(2712.0));
        assert_eq!(back.view.unwrap()["session"]["status"], "busy");
        assert!(
            json.contains(r#""promptAgeSeconds":2712"#),
            "camelCase on the wire: {json}"
        );
    }

    #[test]
    fn a_billed_call_says_so_and_a_cache_hit_does_not() {
        // The whole point of the field. `decidedBy` cannot answer this: the tool-age floor
        // relabels an answered call `floor`, which otherwise reads exactly like a free cache
        // hit, so counting billed calls off the label alone gives a bracket rather than a
        // number.
        let rec = Recorder::new();
        let mut billed = decision("c1", "s1", "working");
        billed.decided_by = "floor".to_string();
        rec.record(billed, true);

        let mut cached = decision("c1", "s2", "working");
        cached.decided_by = "cache".to_string();
        rec.record(cached, false);

        let lines = drain(&rec);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].asked, "a `floor` line can still have cost a call");
        assert!(!lines[1].asked, "the cache answered this one for nothing");

        // On the wire too: without this, a stray `#[serde(skip)]` would pass every other test
        // while writing a log nothing could count.
        let json = serde_json::to_string(&lines[0]).unwrap();
        assert!(json.contains(r#""asked":true"#), "{json}");
        assert!(!serde_json::to_string(&lines[1]).unwrap().contains("asked"));
    }

    #[test]
    fn what_a_call_cost_survives_a_round_trip() {
        let mut d = decision("c1", "s1", "working");
        d.decided_by = "model".to_string();
        d.usage = Some(CallUsage {
            input_tokens: 1913,
            output_tokens: 66,
            cached_input_tokens: Some(1536),
            reasoning_output_tokens: Some(28),
        });
        let json = serde_json::to_string(&d).unwrap();
        let back: Decision = serde_json::from_str(&json).unwrap();
        assert_eq!(back.usage, d.usage);
        assert!(
            json.contains(r#""inputTokens":1913"#),
            "camelCase on the wire: {json}"
        );
        assert!(json.contains(r#""cachedInputTokens":1536"#), "{json}");
    }

    /// The log's own shape, not the provider's: `crate::stuck::codex_answer_text` reads the
    /// endpoint's snake_case fields one at a time out of a `Value`, and what lands here is
    /// already this struct. The two totals are all that endpoint has been observed to send, so
    /// a line carrying only them has to read back as cleanly as a fully detailed one.
    #[test]
    fn a_line_with_only_the_two_totals_reads_back_clean() {
        let usage: CallUsage =
            serde_json::from_str(r#"{"inputTokens":67,"outputTokens":66}"#).unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (67, 66));
        assert_eq!(usage.cached_input_tokens, None);
        assert_eq!(usage.reasoning_output_tokens, None);
        let json = serde_json::to_string(&usage).unwrap();
        assert!(
            !json.contains("cached"),
            "an absent detail is absent, not null: {json}"
        );
    }

    #[test]
    fn a_file_settled_line_carries_no_view_key_at_all() {
        let json = serde_json::to_string(&decision("c1", "s1", "idle")).unwrap();
        assert!(
            !json.contains("view"),
            "an absent view is absent, not null: {json}"
        );
        assert!(!json.contains("promptAgeSeconds"));
        // A line no model call produced is byte-identical to what it was before usage
        // accounting existed: nothing that never asked grows an `asked` or a `usage` key.
        assert!(!json.contains("asked"), "{json}");
        assert!(!json.contains("usage"), "{json}");
    }

    #[test]
    fn flush_appends_ndjson_and_groups_by_the_day_each_line_carries() {
        let dir = scratch("flush");
        let rec = Recorder::new();
        rec.record(decision("c1", "s1", "idle"), false);
        rec.record(decision("c1", "s2", "working"), false);
        // Straddle midnight by hand, which a pass really can do. Both stamps are set so the
        // test says the same thing whatever day it runs on.
        rec.pending.lock().unwrap()[0].ts = "2026-08-04T23:59:59Z".to_string();
        rec.pending.lock().unwrap()[1].ts = "2026-08-05T00:00:01Z".to_string();
        rec.flush(dir.to_str().unwrap());

        let first = std::fs::read_to_string(dir.join("stuck/2026-08-04.ndjson")).unwrap();
        assert_eq!(first.lines().count(), 1);
        let second = std::fs::read_to_string(dir.join("stuck/2026-08-05.ndjson")).unwrap();
        assert!(second.ends_with('\n'), "every record is a whole line");
        assert_eq!(
            serde_json::from_str::<Decision>(second.trim())
                .unwrap()
                .session,
            "s2"
        );

        // Appends rather than truncates: a second pass must not lose the first.
        rec.record(decision("c1", "s1", "working"), false);
        rec.pending.lock().unwrap()[0].ts = "2026-08-05T00:00:02Z".to_string();
        rec.flush(dir.to_str().unwrap());
        let grown = std::fs::read_to_string(dir.join("stuck/2026-08-05.ndjson")).unwrap();
        assert_eq!(grown.lines().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_keeps_the_newest_month_by_name() {
        let dir = scratch("prune");
        // Forty consecutive real days, crossing a month so the sort is doing something.
        for day in 1..=31 {
            std::fs::write(dir.join(format!("2026-01-{day:02}.ndjson")), "{}\n").unwrap();
        }
        for day in 1..=9 {
            std::fs::write(dir.join(format!("2026-02-{day:02}.ndjson")), "{}\n").unwrap();
        }
        std::fs::write(dir.join("notes.txt"), "left alone").unwrap();
        prune(&dir);
        let mut kept: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".ndjson"))
            .collect();
        kept.sort();
        assert_eq!(kept.len(), KEEP_DAYS);
        assert_eq!(kept.first().unwrap(), "2026-01-11.ndjson");
        assert_eq!(kept.last().unwrap(), "2026-02-09.ndjson");
        assert!(dir.join("notes.txt").exists(), "only day files are pruned");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retain_drops_clones_that_left_the_fleet() {
        let rec = Recorder::new();
        rec.record(decision("gone-clone", "s1", "idle"), false);
        rec.record(decision("kept-clone", "s1", "idle"), false);
        drain(&rec);
        rec.retain(&["kept-clone".to_string()].into_iter().collect());
        // A deleted clone leaves no closing line, and its session id is free again.
        rec.record(decision("gone-clone", "s1", "idle"), false);
        let lines = drain(&rec);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].was, "new");
    }
}
