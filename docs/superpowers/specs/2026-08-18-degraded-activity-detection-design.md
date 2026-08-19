# Degraded activity detection when the judge cannot answer

**Status:** design, approved 2026-08-18
**Touches:** `crates/wire/src/control.rs`, `crates/control-server/src/{stuck,monitor}.rs`,
`crates/cli/src/commands.rs`, `frontend/app/{lib,components}/…`

## The problem

`stuck.rs` decides the working/idle dot. Reading files can only produce `Verdict::Stuck` or
`Verdict::Ask`; **only a model answer can produce `Working`** (`apply_verdict`). The model is
GPT, reached on an imported Codex account.

On 2026-08-18 that account hit its weekly quota. Every ask returned HTTP 429, the `Err` arm
mapped each one to `Idle`, and the entire fleet read "not working" for ~36 hours while six clones
were demonstrably running — confirmed against the transcript ledger, which showed 200 records in
five minutes for a clone the dashboard called idle.

Two defects, not one:

1. **No fallback.** Evidence sufficient to light the dot was already on disk and went unused.
2. **`idle` means two different things** — "I know it is idle" and "I cannot tell". Conflating
   them is what makes an outage read as a fleet-wide stop. This is the deeper defect.

The current all-or-nothing behaviour is deliberate (`JudgeConfig.ts`: *"a guess in either
direction is worse than an honest 'not working'"*). This design does not overturn that principle;
it stops applying it to a case it was never about — we are not guessing, we are declining to say.

## Decisions taken

| Question | Decision |
|---|---|
| Shape | Lift what files can prove, mark the rest `unknown` |
| `working → unknown` notification | **Silent.** Replay on recovery |
| Scope | **Outage only.** A rig that never configured a judge is unchanged |
| What counts as an outage | Quota immediately; other failures after 3 consecutive; clears on first success |

The scope decision is load-bearing: "configured but broken" must be distinguishable from "never
configured", and today neither is distinguishable from "healthy".

## Design

### 1. Detecting the outage

`backend()` fails only when there is no imported/named Codex account, or when a token refresh
breaks. **A 429 takes the success path** — the refresh works and the rejection happens later
inside `ask_codex`. Worse, `note_gap("")` is then called, which returns false *and clears any
prior gap*, so the standing-condition warning is silent for the whole outage.

Add a `JudgeHealth` latch on `Judge` (which already owns an `StdRwLock`):

- `error_line`'s `ApiErrorBody` already parses `type`, `plan_type`, `resets_at` and flattens them
  into prose. Return the structured kind alongside the error instead, so the `Err` arm can match
  `usage_limit_reached` specifically rather than string-grepping.
- **Quota** (`usage_limit_reached`) engages degraded mode on the first occurrence, carrying
  `resets_at` as the expected duration. It is the only failure that tells us it will still be
  true in an hour.
- **Any other ask failure** increments a counter; degraded engages at 3 consecutive failures, so
  a single dropped connection cannot darken 20 clones.
- **Any successful ask** clears the latch and the counter.
- Degraded requires a *configured* judge, and `backend()` currently returns `None` for two
  reasons that the outage-only scope must separate:
  - **No account imported / no account matching `judge.codexEmail`** (`judge_account` fails) —
    never configured. Behaviour is exactly as today: `Idle`, `decidedBy: "no-judge"`. This is
    what keeps the change invisible to installations that never had a judge.
  - **Token refresh failed** (`fresh_access_token` fails) — configured but broken. This *is* an
    outage and engages degraded mode, on the same 3-failure threshold as any other non-quota
    failure.

  `backend()` returns `(Option<Backend>, String)` today, so the caller can only tell these apart
  by parsing prose. It returns a typed reason instead — an enum distinguishing *absent* from
  *broken* — and `note_gap` keeps taking the rendered string for its existing warning.

Once latched, skip the per-clone HTTP round trip entirely. Today the outage *increases* request
volume: a failed ask is not cached (`stuck.rs:1546-1547`) and the per-clone walk only short-
circuits on a `Working` answer, so every Ask session re-POSTs its full body every 4 seconds for
36 hours. Latching stops that storm as a side effect.

### 2. Deciding a session while degraded

Only `Verdict::Ask` sessions are affected. A session the files already settled (`status` of
`idle` or `waiting`) still reads `Idle` — those are certain and unchanged.

For an `Ask` session, in order:

1. `view["session"]["generating"] == true` → **Working**. `generating` is `status == "busy" &&
   !blocked`; the judge's own system prompt already treats it as decisive without deliberation
   (*"If generating is true, that alone means true"*).
2. `overruled(Idle, &case.view)` lifts → **Working**. The tool-age floor: the main agent entered
   a machine-answered tool call less than `HANG_GRACE_S` (60s) ago, excluding `HUMAN_WAITS`.
   Already a pure function of the view, already computed, today called from exactly one site
   inside the `Ok` arm.
3. Otherwise → **Unknown**.

Neither rule is new. Both are assertions the module already trusts; the change is ungating them
from the model path. Both are *lift-only* — they can turn `Idle` into `Working` and never the
reverse — so degraded mode cannot manufacture a false "not working". It can only manufacture a
false "working", bounded to ~60s per tool call by `HANG_GRACE_S`.

**What this recovers:** agents actively emitting tokens, and agents in the first minute of a tool
call. **What stays unknown:** every agent inside a `cargo build`, test run, `ssh`, or long `Task`
past 60 seconds — which is most of a working agent's wall-clock time. This produces a *partial*
answer, honestly labelled, not a correct fleet.

### 3. The fourth state

`MonitorState` gains `Unknown` (`wire/src/control.rs`), serialised `"unknown"`. `MonitorState.ts`
regenerates via ts-rs; `frontend/app/lib/types.ts` carries a hand-written duplicate union that
must be updated by hand. Both dot maps are exhaustive `Record`s, so `tsc` catches omissions.

State-sensitive logic in `monitor.rs`, each decided explicitly:

| Site | Behaviour with `Unknown` |
|---|---|
| `debounce` | Unchanged — holds only `working → idle`. Degraded is latched so it cannot flap, and the transition is silent anyway. |
| `should_flag_unread` | Returns **false** for `Unknown`. This is what makes the outage silent. |
| `lift_sub_clone_activity` | Unchanged — only a `Working` sub lifts a parent. An `unknown` sub proves nothing. |
| `pick_stat` | Unchanged — already keys on `!= Offline`. |
| `ensure_autonomous_listener` | Unchanged — already keys on `!= Offline`. |
| CLI `clone_status` | Gains `"unknown"`. |

### 4. Replay on recovery

`unread` fires on `working → not-working`. Once a clone sits at `Unknown`, its stored state is no
longer `Working`, so recovery into `Idle` would raise nothing — the stop would be swallowed.

Track the state each clone held when it entered `Unknown`, refreshed each time it re-enters. On
`Unknown → Idle` **or `Unknown → Offline`**, apply `should_flag_unread` against *that* remembered
state. A clone that was working when the judge died and is genuinely idle when it returns raises
then; one that was already idle does not. `Unknown → Working` raises nothing, as today.

The `Offline` case matters on its own: a container that dies mid-outage must not be swallowed,
and `should_flag_unread` already surfaces an offline transition even when recently viewed.

Kept in memory beside `LastSeen`, not persisted: a restart during an outage legitimately loses
the baseline, and inventing one would be worse than losing it.

No artificial cap on the replay batch. It is bounded by the clones that were working at outage
start *and* stopped during it, and `should_flag_unread` already suppresses any the operator has
viewed since. An artificial cap would drop real events with no rule for choosing which.

### 5. Telling the operator

The per-clone dot says `unknown`; the *reason* needs a fleet-level surface. `judge.codexEmail`
and the Codex account's `sevenDay {pct, resetsAt}` already reach the browser, so a banner is
derivable from data on the wire today with **no backend change**. Out of scope for this change;
the dot is the deliverable, the banner is a follow-up.

## What does not change

- A rig with no Codex account: identical behaviour, `decidedBy: "no-judge"`, nothing green.
- Sessions settled from files (`idle`, `waiting`): still `Idle`.
- The healthy path: when the judge answers, every verdict comes from the model exactly as today.
  The lift rules run only while latched.
- `Verdict` itself. `Working` is still never produced by `read_clone`; the lift happens in
  `resolve_fleet`, next to `overruled`'s existing call site.

## Testing

Unit, in `stuck.rs`:
- `generating: true` lifts to Working while degraded; `false` does not.
- The tool-age floor lifts while degraded; a `HUMAN_WAITS` tool does not; a call ≥60s does not.
- A session with `status: "waiting"` still reads `Idle` while degraded — the lift never reaches it.
- Quota error latches on the first failure; a non-quota error does not latch until the third;
  any success clears both.
- With no account imported, degraded never engages and the `no-judge` line is unchanged.

Unit, in `monitor.rs`:
- `should_flag_unread` returns false for `Unknown`.
- `working → unknown → idle` raises unread once, at the second transition.
- `idle → unknown → idle` raises nothing.
- `debounce` leaves `working → unknown` unheld.

Frontend: `tsc` proves exhaustiveness; a story for the new dot.

## Risks

**A false green.** The only measured precedent for a file-only rule on this fleet
(`docs/API.md:369`) missed **250 stuck clones against 20 false alarms** — it over-reported
working, with a median 331s to notice a stall. That was a different rule (5-minute transcript
inactivity, pooled at clone level), so the numbers do not transfer, but the direction is a
warning. Mitigation: both lift rules are bounded and self-limiting, and `unknown` — not
`working` — is the default for everything they cannot prove.

**`busy`/`shell` semantics are undocumented internals.** `template/setup/30-user.sh` installs
Claude Code unpinned, so these can drift with any auto-update. `generating` depends on `busy`.
The tool-age floor does not, which is why it is kept as an independent second rule rather than
folded into the first.

**A fourth state is a wire change.** Any consumer switching exhaustively on `MonitorState`
outside this repo breaks. Nothing in-tree does after the changes above; external CLI consumers
parsing `rmng` output would see a new word.
