# Editable Agent Playbook Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the desktop agent's baked-in instructions editable from the control-server Settings UI (a global playbook + optional per-preset append), injected into each new clone at creation.

**Architecture:** Merge the wrapper's two instruction files into one canonical `agent-wrapper/agent-instructions.md`, embedded by both the wrapper (baked-in fallback) and the control-server (seed default via `include_str!`). A new `agentPlaybook` field on `AppConfig` (seeded default) and an optional one on each `Preset` (append) are edited in Settings; at clone creation the composed text is injected as `~/.config/rmng/agent-instructions.md`, which the wrapper reads (falling back to the baked-in default).

**Tech Stack:** Rust (control-server + `wire` crate, `ts-rs` for TS bindings), TypeScript/Bun (agent-wrapper), React + Tailwind (frontend), Docker, Proxmox LXC (E2E).

## Global Constraints

- **No `-e` config flags** — `config.json` (edited via Settings/wizard) is the single source of truth. New settings are config fields, never env overrides on the control-server.
- **Field naming:** new fields are `agent_playbook` (Rust) / `agentPlaybook` (TS/JSON). Do **not** touch the existing per-clone `agent_instructions`/`claude_instructions` (a different layer — folded into the first task message).
- **Not restart-required:** `agentPlaybook` applies to the *next* clone (read fresh per clone). Do not add it to `restart_required()` or `enforce_categories()`.
- **Secrets:** `agentPlaybook` is non-secret on both `AppConfig` and `Preset` — it passes through the redacted views verbatim (like `vars`/`labels`).
- **Composition (verbatim from spec):** effective text = `global agentPlaybook` + `"\n\n"` + `preset agentPlaybook` (join skipped when the preset field is empty), mirroring the wrapper's current `[notes, procedure].filter(Boolean).join("\n\n")`.
- **Injected file path:** `home/<CLONE_USER>/.config/rmng/agent-instructions.md`, mode `0644`, uid/gid `1000` (`CLONE_UID`/`CLONE_GID`).
- **ts-rs regeneration:** after changing any `#[ts(export)]` struct, regenerate the `frontend/app/lib/wire/*.ts` files with `cargo test -p wire` (ts-rs writes bindings during its export tests).

---

## File Structure

**agent-wrapper (Bun/TS):**
- Create `agent-wrapper/agent-instructions.md` — merged canonical default (single source of truth).
- Delete `agent-wrapper/operating-notes.md`, `agent-wrapper/ticket-procedure.md`.
- Create `agent-wrapper/src/instructions.ts` — baked-in default + `resolveSystemAppend()` (pure, testable; no server side effects).
- Create `agent-wrapper/src/instructions.test.ts` — `bun test` for the resolver.
- Modify `agent-wrapper/src/config.ts` — add `instructionsPath`.
- Modify `agent-wrapper/src/server.ts` — import from `instructions.ts`, drop the two `.md` imports + join.

**wire crate (Rust, TS source of truth):**
- Modify `crates/wire/src/config.rs` — `agent_playbook` on `AppConfig`/`AppConfigRedacted`/`Preset`/`PresetRedacted`, `default_agent_playbook()`, redaction, tests.

**control-server (Rust):**
- Modify `crates/control-server/src/config.rs` — `merge_presets()` carries `agentPlaybook`.
- Modify `crates/control-server/src/web.rs` — `compose_playbook()` + set `CloneSpec.agent_playbook` on both clone paths.
- Modify `crates/control-server/src/jobs.rs` — `CloneSpec.agent_playbook` + pass to `clone_container`.
- Modify `crates/control-server/src/provision.rs` — `clone_container`/`clone_container_after_create` gain `agent_playbook`; inject the tar entry.

**frontend (React/TS):**
- Modify `frontend/app/components/SettingsPanel.tsx` — global textarea Section + per-preset textarea + payload.
- Modify `frontend/app/stories/fixtures.ts` — add `agentPlaybook` to the `appConfig` fixture + its preset.

**docs:**
- Modify `agent-wrapper/README.md`, `docs/PROTOCOL.md`, `docs/API.md`.

---

## Task 1: agent-wrapper — merge instruction files + read the injected file

**Files:**
- Create: `agent-wrapper/agent-instructions.md`, `agent-wrapper/src/instructions.ts`, `agent-wrapper/src/instructions.test.ts`
- Delete: `agent-wrapper/operating-notes.md`, `agent-wrapper/ticket-procedure.md`
- Modify: `agent-wrapper/src/config.ts`, `agent-wrapper/src/server.ts`

**Interfaces:**
- Produces: `resolveSystemAppend(injectedPath: string, read?: (p: string) => string): string`, `BAKED_IN_INSTRUCTIONS: string` (both from `src/instructions.ts`); `CONFIG.instructionsPath: string`.

- [ ] **Step 1: Merge the two markdown files into one, delete the originals**

Run (deterministic concat — notes first, blank line, then procedure, matching today's `SYSTEM_APPEND` order):
```bash
cd agent-wrapper
{ cat operating-notes.md; echo; cat ticket-procedure.md; } > agent-instructions.md
git rm operating-notes.md ticket-procedure.md
git add agent-instructions.md
```

- [ ] **Step 2: Write the failing test** (`agent-wrapper/src/instructions.test.ts`)

```ts
import { expect, test } from "bun:test";
import { resolveSystemAppend, BAKED_IN_INSTRUCTIONS } from "./instructions";

test("baked-in default is the merged instructions file, trimmed and non-empty", () => {
  expect(BAKED_IN_INSTRUCTIONS.length).toBeGreaterThan(0);
  expect(BAKED_IN_INSTRUCTIONS).toBe(BAKED_IN_INSTRUCTIONS.trim());
});

test("a non-empty injected file wins over the baked-in default", () => {
  const injected = "# Custom playbook\nDo the custom thing.";
  const read = () => injected;
  expect(resolveSystemAppend("/any/path", read)).toBe(injected);
});

test("a missing/unreadable file falls back to the baked-in default", () => {
  const read = () => {
    throw new Error("ENOENT");
  };
  expect(resolveSystemAppend("/nope", read)).toBe(BAKED_IN_INSTRUCTIONS);
});

test("an empty/whitespace injected file falls back to the baked-in default", () => {
  expect(resolveSystemAppend("/x", () => "   \n  ")).toBe(BAKED_IN_INSTRUCTIONS);
});
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd agent-wrapper && bun test src/instructions.test.ts`
Expected: FAIL — `Cannot find module './instructions'`.

- [ ] **Step 4: Write `src/instructions.ts`**

```ts
// The desktop agent's baked-in playbook (operating notes + ticket procedure), embedded at
// BUILD time via a Bun text import so it always ships inside the `bun build --compile`
// single-exec (a runtime read of a bunfs-relative path would ENOENT). This is the FALLBACK:
// at clone creation the control-server injects an editable copy at CONFIG.instructionsPath,
// which wins when present. See agent-wrapper/README.md.
import BAKED_IN_RAW from "../agent-instructions.md" with { type: "text" };
import { readFileSync } from "node:fs";

export const BAKED_IN_INSTRUCTIONS = BAKED_IN_RAW.trim();

/** The system-prompt append: the injected file if present + non-empty, else the baked-in
 *  default. `read` is injectable for testing; defaults to a UTF-8 file read. */
export function resolveSystemAppend(
  injectedPath: string,
  read: (p: string) => string = (p) => readFileSync(p, "utf8"),
): string {
  try {
    const injected = read(injectedPath).trim();
    if (injected) return injected;
  } catch {
    // absent / unreadable (local `bun run` dev, or robustness) — use the baked-in default
  }
  return BAKED_IN_INSTRUCTIONS;
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd agent-wrapper && bun test src/instructions.test.ts`
Expected: PASS (4 tests).

- [ ] **Step 6: Add `instructionsPath` to `src/config.ts`**

Insert into the `CONFIG` object (e.g. after `linearApiKey`):
```ts
  /** Editable agent playbook injected by the control-server at clone creation. The wrapper
   * reads this at startup; absent ⇒ the baked-in default (see instructions.ts). */
  instructionsPath:
    process.env.AGENT_INSTRUCTIONS_PATH ??
    `${process.env.HOME ?? "/home/rmng"}/.config/rmng/agent-instructions.md`,
```

- [ ] **Step 7: Rewire `src/server.ts` to use the resolver**

Replace the two text imports ([lines 38-39](../../../agent-wrapper/src/server.ts#L38-L39)):
```ts
import OPERATING_NOTES_RAW from "../operating-notes.md" with { type: "text" };
import TICKET_PROCEDURE_RAW from "../ticket-procedure.md" with { type: "text" };
```
with:
```ts
import { resolveSystemAppend } from "./instructions";
```
Then replace the `TICKET_PROCEDURE`/`OPERATING_NOTES`/`SYSTEM_APPEND` block ([lines 43-56](../../../agent-wrapper/src/server.ts#L43-L56)) with:
```ts
// The system-prompt append for THIS host's session agent: the control-server-injected,
// Settings-editable playbook (operating notes + ticket procedure) if present, else the
// baked-in default. Read once at startup — a fresh clone boots a fresh wrapper. It is NOT
// placed in ~/.claude/CLAUDE.md: the inner Cursor Claude Code reads that file and would
// recursively try to open Cursor. See instructions.ts + agent-wrapper/README.md.
const SYSTEM_APPEND = resolveSystemAppend(CONFIG.instructionsPath);
```
(The `buildOptions` use of `SYSTEM_APPEND` at [line 187](../../../agent-wrapper/src/server.ts#L187) is unchanged — `{ append }` is still spread only when non-empty.)

- [ ] **Step 8: Typecheck**

Run: `cd agent-wrapper && bun run typecheck`
Expected: no errors.

- [ ] **Step 9: Commit**

```bash
git add agent-wrapper/
git commit -m "feat(agent-wrapper): read Settings-editable playbook, merge the two instruction files"
```

---

## Task 2: wire — `agentPlaybook` config fields + seeded default

**Files:**
- Modify: `crates/wire/src/config.rs`
- Modify (generated): `frontend/app/lib/wire/AppConfigRedacted.ts`, `frontend/app/lib/wire/PresetRedacted.ts`

**Interfaces:**
- Consumes: `agent-wrapper/agent-instructions.md` (from Task 1) via `include_str!`.
- Produces: `AppConfig.agent_playbook: String`, `Preset.agent_playbook: String`, `AppConfigRedacted.agent_playbook: String`, `PresetRedacted.agent_playbook: String`, `wire::config::default_agent_playbook() -> String`.

- [ ] **Step 1: Write the failing tests** — add to the `tests` module in `crates/wire/src/config.rs`:

```rust
#[test]
fn agent_playbook_defaults_to_embedded_file() {
    // Missing key ⇒ the shipped default (the merged wrapper instructions), non-empty.
    let c: AppConfig = serde_json::from_str("{}").unwrap();
    assert!(!c.agent_playbook.is_empty());
    assert_eq!(c.agent_playbook, default_agent_playbook());
    // A preset's playbook defaults to empty (optional append).
    let p: Preset = serde_json::from_str(r#"{ "name": "x" }"#).unwrap();
    assert!(p.agent_playbook.is_empty());
}

#[test]
fn agent_playbook_passes_through_redaction() {
    let c = AppConfig {
        agent_playbook: "GLOBAL NOTES".into(),
        presets: vec![Preset {
            name: "p".into(),
            agent_playbook: "PRESET APPEND".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let r = c.redacted();
    assert_eq!(r.agent_playbook, "GLOBAL NOTES");
    assert_eq!(r.presets[0].agent_playbook, "PRESET APPEND");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p wire agent_playbook`
Expected: FAIL — `no field agent_playbook` / `cannot find function default_agent_playbook`.

- [ ] **Step 3: Add the field + default fn + redaction (implementation)**

In `crates/wire/src/config.rs`:

(a) `Preset` struct — add after `vars`:
```rust
    /// Optional per-preset text appended (after `"\n\n"`) to the global agent playbook for
    /// clones of this preset. Empty ⇒ no append. Non-secret.
    #[serde(default)]
    pub agent_playbook: String,
```

(b) `Preset::redacted()` — add to the `PresetRedacted { ... }` literal:
```rust
            agent_playbook: self.agent_playbook.clone(),
```

(c) `PresetRedacted` struct — add:
```rust
    pub agent_playbook: String,
```

(d) `AppConfig` struct — add after `detector_inference_url` (or anywhere in the struct):
```rust
    /// The desktop agent's base playbook (operating notes + ticket procedure), injected into
    /// each new clone at creation as its system-prompt append. Seeded with the shipped default
    /// (the wrapper's `agent-instructions.md`); edited in Settings. Applies to the next clone.
    #[serde(default = "default_agent_playbook")]
    pub agent_playbook: String,
```

(e) `AppConfig::default()` — add:
```rust
            agent_playbook: default_agent_playbook(),
```

(f) The default fn (next to the other `default_*` fns):
```rust
/// The shipped agent playbook: the wrapper's merged instructions file, embedded so the
/// control-server can seed the setting and inject it without a runtime file dependency.
/// Same file the agent-wrapper bakes in as its fallback (single source of truth).
fn default_agent_playbook() -> String {
    include_str!("../../../agent-wrapper/agent-instructions.md").to_string()
}
```
(Make it `pub` if a test outside the module references it; the in-module test above does not need `pub`.)

(g) `AppConfig::redacted()` — add to the `AppConfigRedacted { ... }` literal:
```rust
            agent_playbook: self.agent_playbook.clone(),
```

(h) `AppConfigRedacted` struct — add:
```rust
    pub agent_playbook: String,
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `cargo test -p wire agent_playbook`
Expected: PASS (2 tests).

- [ ] **Step 5: Regenerate the TS bindings + confirm the full wire suite is green**

Run: `cargo test -p wire`
Expected: PASS; `frontend/app/lib/wire/AppConfigRedacted.ts` and `PresetRedacted.ts` now contain `agentPlaybook: string`.

Verify:
```bash
grep agentPlaybook frontend/app/lib/wire/AppConfigRedacted.ts frontend/app/lib/wire/PresetRedacted.ts
```
Expected: a match in each file.

- [ ] **Step 6: Commit**

```bash
git add crates/wire/src/config.rs frontend/app/lib/wire/AppConfigRedacted.ts frontend/app/lib/wire/PresetRedacted.ts
git commit -m "feat(wire): agentPlaybook config field (global + per-preset), seeded default"
```

---

## Task 3: control-server — merge, compose, and inject the playbook

**Files:**
- Modify: `crates/control-server/src/config.rs` (`merge_presets`)
- Modify: `crates/control-server/src/web.rs` (`compose_playbook` + `CloneSpec` construction)
- Modify: `crates/control-server/src/jobs.rs` (`CloneSpec.agent_playbook` + `clone_container` call)
- Modify: `crates/control-server/src/provision.rs` (`clone_container` signature + tar entry)

**Interfaces:**
- Consumes: `wire::Preset.agent_playbook`, `wire::AppConfig.agent_playbook` (Task 2).
- Produces: `CloneSpec.agent_playbook: String`; `clone_container(app, image, hostname, env, agent_playbook: &str, on_progress)`.

- [ ] **Step 1: Write the failing test for `merge_presets` carrying `agentPlaybook`** — add to the `tests` module in `crates/control-server/src/config.rs`:

```rust
#[test]
fn merge_carries_preset_agent_playbook() {
    let base = AppConfig::default();
    let incoming = serde_json::json!({
        "presets": [{ "name": "p", "agentPlaybook": "extra for p", "linearKey": "" }],
    });
    let merged = merge_update(&base, incoming).unwrap();
    assert_eq!(merged.presets.len(), 1);
    assert_eq!(merged.presets[0].agent_playbook, "extra for p");
}

#[test]
fn merge_sets_global_agent_playbook() {
    let base = AppConfig::default();
    let incoming = serde_json::json!({ "agentPlaybook": "NEW GLOBAL" });
    let merged = merge_update(&base, incoming).unwrap();
    assert_eq!(merged.agent_playbook, "NEW GLOBAL");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p control-server merge_carries_preset_agent_playbook merge_sets_global_agent_playbook`
Expected: FAIL — `merged.presets[0].agent_playbook` is empty (merge_presets drops it).

- [ ] **Step 3: Implement `merge_presets` carrying the field** — in `crates/control-server/src/config.rs`, inside `merge_presets`, read the field and add it to the constructed `Preset`:

Add before the `out.push(...)`:
```rust
        let agent_playbook =
            r.get("agentPlaybook").and_then(|v| v.as_str()).unwrap_or("").to_string();
```
Change the push from:
```rust
        out.push(wire::Preset { name, labels, linear_key, vars });
```
to:
```rust
        out.push(wire::Preset { name, labels, linear_key, vars, agent_playbook });
```
(The global `agentPlaybook` needs no code here — `deep_merge` already overlays it; `merge_sets_global_agent_playbook` passes once the field exists on `AppConfig`.)

- [ ] **Step 4: Run to verify the merge tests pass**

Run: `cargo test -p control-server merge_carries_preset_agent_playbook merge_sets_global_agent_playbook`
Expected: PASS.

- [ ] **Step 5: Write the failing test for `compose_playbook`** — add a `tests` module (or extend one) in `crates/control-server/src/web.rs`:

```rust
#[cfg(test)]
mod playbook_tests {
    use super::*;

    fn cfg_with(global: &str) -> wire::AppConfig {
        wire::AppConfig { agent_playbook: global.into(), ..Default::default() }
    }
    fn preset_with(pb: &str) -> wire::Preset {
        wire::Preset { name: "p".into(), agent_playbook: pb.into(), ..Default::default() }
    }

    #[test]
    fn global_only_when_no_preset() {
        assert_eq!(compose_playbook(&cfg_with("BASE"), None), "BASE");
    }

    #[test]
    fn global_only_when_preset_field_empty() {
        assert_eq!(compose_playbook(&cfg_with("BASE"), Some(&preset_with("  "))), "BASE");
    }

    #[test]
    fn appends_preset_after_global_with_blank_line() {
        assert_eq!(
            compose_playbook(&cfg_with("BASE"), Some(&preset_with("EXTRA"))),
            "BASE\n\nEXTRA"
        );
    }
}
```

- [ ] **Step 6: Run to verify failure**

Run: `cargo test -p control-server playbook_tests`
Expected: FAIL — `cannot find function compose_playbook`.

- [ ] **Step 7: Implement `compose_playbook`** — in `crates/control-server/src/web.rs`, next to `preset_env`:

```rust
/// The effective agent playbook for a clone: the global `agentPlaybook` plus the preset's
/// optional append (after a blank line). Empty/whitespace preset field ⇒ global only. Mirrors
/// the wrapper's `[notes, procedure].filter(Boolean).join("\n\n")`.
fn compose_playbook(cfg: &wire::AppConfig, preset: Option<&wire::Preset>) -> String {
    let base = cfg.agent_playbook.trim();
    match preset.map(|p| p.agent_playbook.trim()).filter(|s| !s.is_empty()) {
        Some(extra) => format!("{base}\n\n{extra}"),
        None => base.to_string(),
    }
}
```

- [ ] **Step 8: Wire it into both `CloneSpec` constructions** — in `crates/control-server/src/web.rs`:

Plain path (the `CloneSpec { ... }` near [line 341](../../../crates/control-server/src/web.rs#L341)) — add:
```rust
            agent_playbook: compose_playbook(&cfg, explicit),
```
Ticket path (the `CloneSpec { ... }` near [line 371](../../../crates/control-server/src/web.rs#L371)) — add:
```rust
        agent_playbook: compose_playbook(&cfg, Some(&preset)),
```

- [ ] **Step 9: Add the `CloneSpec` field** — in `crates/control-server/src/jobs.rs`, add to `CloneSpec` (after `env`, distinct from the existing `agent_instructions: Option<String>`):

```rust
    /// Composed agent playbook (global + preset append) injected into the clone at creation
    /// as ~/.config/rmng/agent-instructions.md. Empty ⇒ no file injected.
    pub agent_playbook: String,
```

- [ ] **Step 10: Pass it to `clone_container`** — in `crates/control-server/src/jobs.rs` `run_clone`, change the call ([line 289](../../../crates/control-server/src/jobs.rs#L289)) to add `&spec.agent_playbook` before `progress`:

```rust
    let image_ref =
        match clone_container(&app, &spec.source_image, &spec.new_hostname, &env, &spec.agent_playbook, progress).await {
```

- [ ] **Step 11: Thread through `provision.rs` + inject the tar entry** — in `crates/control-server/src/provision.rs`:

(a) `clone_container` signature ([line 253](../../../crates/control-server/src/provision.rs#L253)) — add `agent_playbook: &str` before `on_progress`:
```rust
pub async fn clone_container(
    app: &App,
    image: &str,
    hostname: &str,
    env: &[EnvVar],
    agent_playbook: &str,
    mut on_progress: impl FnMut(&str, &str),
) -> Result<String> {
```

(b) The call to `clone_container_after_create` ([line 303](../../../crates/control-server/src/provision.rs#L303)) — pass it through:
```rust
    match clone_container_after_create(app, &container, hostname, env, agent_playbook, &mut on_progress).await {
```

(c) `clone_container_after_create` signature ([line 317](../../../crates/control-server/src/provision.rs#L317)) — add the param:
```rust
async fn clone_container_after_create(
    app: &App,
    container: &str,
    hostname: &str,
    env: &[EnvVar],
    agent_playbook: &str,
    on_progress: &mut impl FnMut(&str, &str),
) -> Result<()> {
```

(d) Inject the file — after the preset-env `TarEntry` is pushed into `entries` (the `vec![...]` ending near [line 350](../../../crates/control-server/src/provision.rs#L350)), add:
```rust
    // The Settings-editable agent playbook (global + preset append), read by the agent-wrapper
    // at startup (AGENT_INSTRUCTIONS_PATH). Empty ⇒ skip; the wrapper then uses its baked-in
    // default. Distinct from environment.d (this is a multi-KB markdown blob, not a KEY=VALUE).
    if !agent_playbook.trim().is_empty() {
        entries.push(TarEntry {
            path: format!("home/{CLONE_USER}/.config/rmng/agent-instructions.md"),
            data: agent_playbook.as_bytes().to_vec(),
            mode: 0o644,
            uid: CLONE_UID,
            gid: CLONE_GID,
        });
    }
```

- [ ] **Step 12: Build + run the full control-server test suite**

Run: `cargo test -p control-server`
Expected: PASS (incl. the new merge + playbook tests). Fix any remaining `CloneSpec { .. }` literals the compiler flags as missing `agent_playbook` (only the two in `web.rs` construct it; `Default` covers the rest via `#[derive(Default)]`).

- [ ] **Step 13: Commit**

```bash
git add crates/control-server/
git commit -m "feat(control-server): compose + inject the agent playbook into new clones"
```

---

## Task 4: frontend — Settings UI (global textarea + per-preset append)

**Files:**
- Modify: `frontend/app/components/SettingsPanel.tsx`
- Modify: `frontend/app/stories/fixtures.ts`

**Interfaces:**
- Consumes: `AppConfigRedacted.agentPlaybook`, `PresetRedacted.agentPlaybook` (Task 2 regenerated TS).

- [ ] **Step 1: Add the fixture fields (makes the type change compile)** — in `frontend/app/stories/fixtures.ts`, in the `appConfig` object add a top-level line (e.g. after `detectorInferenceUrl`):
```ts
  agentPlaybook: "# Desktop agent — operating notes\n\n(sample playbook)\n",
```
and in its `presets[0]` object add:
```ts
      agentPlaybook: "",
```

- [ ] **Step 2: Add global playbook state + seed it** — in `frontend/app/components/SettingsPanel.tsx`:

Add state (near [line 225](../../../frontend/app/components/SettingsPanel.tsx#L225)):
```tsx
  const [agentPlaybook, setAgentPlaybook] = useState("");
```
Seed in `load()` (near [line 250](../../../frontend/app/components/SettingsPanel.tsx#L250)):
```tsx
    setAgentPlaybook(c.agentPlaybook);
```
Extend the preset state type ([line 199](../../../frontend/app/components/SettingsPanel.tsx#L199)) with `agentPlaybook: string;` and map it in the `setPresets(...)` seed ([line 252](../../../frontend/app/components/SettingsPanel.tsx#L252)):
```tsx
        agentPlaybook: p.agentPlaybook,
```
Extend `addPreset` ([line 281](../../../frontend/app/components/SettingsPanel.tsx#L281)) to include `agentPlaybook: ""` in the new-preset object, and widen `setPresetField`'s `field` union ([line 287](../../../frontend/app/components/SettingsPanel.tsx#L287)) to include `"agentPlaybook"`:
```tsx
  const setPresetField = (i: number, field: "name" | "labels" | "linearKey" | "agentPlaybook", v: string) =>
```

- [ ] **Step 3: Add the two fields to the PUT payload** — in the payload object (near [line 348](../../../frontend/app/components/SettingsPanel.tsx#L348)) add:
```tsx
        agentPlaybook,
```
and in the `presets.map(...)` ([line 351](../../../frontend/app/components/SettingsPanel.tsx#L351)) add to each mapped object:
```tsx
            agentPlaybook: p.agentPlaybook,
```

- [ ] **Step 4: Render the global Section** — add a new `<Section>` (e.g. just before the "Presets" Section at [line 476](../../../frontend/app/components/SettingsPanel.tsx#L476)):
```tsx
            <Section
              title="Agent instructions"
              effect="immediate"
              hint="The desktop agent's operating notes + ticket procedure, injected as its system prompt. Applies to newly created clones (existing clones keep the instructions they were created with)."
            >
              <textarea
                value={agentPlaybook}
                onChange={(e) => setAgentPlaybook(e.target.value)}
                spellCheck={false}
                rows={16}
                className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100"
              />
            </Section>
```

- [ ] **Step 5: Render the per-preset textarea** — inside the preset card, after the vars block's `+ Add variable` button (near [line 552](../../../frontend/app/components/SettingsPanel.tsx#L552), still inside the preset `<div>`), add:
```tsx
                    <div className="mt-2">
                      <Field label="Extra agent instructions (appended after the global instructions for this preset)">
                        <textarea
                          value={p.agentPlaybook}
                          onChange={(e) => setPresetField(i, "agentPlaybook", e.target.value)}
                          spellCheck={false}
                          rows={4}
                          placeholder="(optional)"
                          className="w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 font-mono text-xs focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
                        />
                      </Field>
                    </div>
```

- [ ] **Step 6: Typecheck the frontend**

Run: `cd frontend && npx tsc --noEmit` (or the repo's typecheck script if present — check `frontend/package.json`).
Expected: no errors (the fixture + payload now satisfy the regenerated types).

- [ ] **Step 7: Commit**

```bash
git add frontend/app/components/SettingsPanel.tsx frontend/app/stories/fixtures.ts
git commit -m "feat(frontend): edit the agent playbook (global + per-preset) in Settings"
```

---

## Task 5: docs — reflect the editable, injected playbook

**Files:**
- Modify: `agent-wrapper/README.md`, `docs/PROTOCOL.md`, `docs/API.md`

- [ ] **Step 1: Update `agent-wrapper/README.md`** — rewrite the "instructions come in two layers" section so it says: the operating notes + ticket procedure are now one file (`agent-instructions.md`) baked in as the **fallback**; the control-server injects a Settings-editable copy at `AGENT_INSTRUCTIONS_PATH` (default `~/.config/rmng/agent-instructions.md`) that wins when present; a per-preset append is concatenated by the control-server before injection. Add an `AGENT_INSTRUCTIONS_PATH` row to the Config (environment) table.

- [ ] **Step 2: Update `docs/PROTOCOL.md` + `docs/API.md`** — wherever `AppConfig`/preset fields or `PUT /api/config` are enumerated, add `agentPlaybook` (global: seeded default, injected into new clones; per-preset: optional append). Note it is non-secret and not restart-required.

- [ ] **Step 3: Commit**

```bash
git add agent-wrapper/README.md docs/PROTOCOL.md docs/API.md
git commit -m "docs: editable agent playbook (agentPlaybook + AGENT_INSTRUCTIONS_PATH)"
```

---

## Task 6: E2E — provision a Proxmox CT, run the stack, validate the injected file

**Goal:** On a freshly provisioned Proxmox LXC (root@10.0.0.100), build the changed control-server image, run it against a nested Docker daemon, set a distinctive `agentPlaybook` (global + a preset append) via the real API, create a clone, and prove the composed text lands at `~/.config/rmng/agent-instructions.md` inside the clone with the right owner/content — and that `config.json` persisted the setting. No Claude credentials are needed: the assertion is on **file placement + content**, which the control-server writes at the `inject` step (before the clone-daemon `wait-ready`), so it holds even if the desktop never fully comes up on a GPU-less CT.

**Environment facts (verified):** PVE 9.2 at `root@10.0.0.100`; templates `local:vztmpl/ubuntu-24.04-standard_24.04-2_amd64.tar.zst`; storage `local-lvm` (rootfs) + `local` (templates); free CT ids from 122. The W6800 GPU is a **runtime** requirement for video encode only — clone creation + file injection are GPU-free.

- [ ] **Step 1: Provision the CT (Docker-capable LXC)**

Run (adjust `CTID` if 122 is taken; `nesting=1`+`keyctl=1` are required for Docker-in-LXC):
```bash
ssh root@10.0.0.100 'CTID=122; \
  pct create $CTID local:vztmpl/ubuntu-24.04-standard_24.04-2_amd64.tar.zst \
    --hostname rmng-playbook-e2e --cores 8 --memory 8192 --swap 4096 \
    --rootfs local-lvm:40 --net0 name=eth0,bridge=vmbr0,ip=dhcp \
    --features nesting=1,keyctl=1 --unprivileged 1 --onboot 0 && \
  pct start $CTID && sleep 5 && pct exec $CTID -- bash -lc "ip -4 addr show eth0 | grep inet"'
```
Expected: the CT starts and prints its DHCP IPv4.

- [ ] **Step 2: Install Docker + raise the keyring sysctls (a known clone-daemon prerequisite)**

```bash
ssh root@10.0.0.100 'CTID=122; pct exec $CTID -- bash -lc "\
  apt-get update && apt-get install -y ca-certificates curl git rsync jq && \
  install -m 0755 -d /etc/apt/keyrings && \
  curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc && \
  chmod a+r /etc/apt/keyrings/docker.asc && \
  echo \"deb [arch=\$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu \$(. /etc/os-release; echo \$VERSION_CODENAME) stable\" > /etc/apt/sources.list.d/docker.list && \
  apt-get update && apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin && \
  sysctl -w kernel.keys.maxkeys=2000 kernel.keys.maxbytes=2000000 && \
  systemctl enable --now docker && docker run --rm hello-world | grep -q Hello && echo DOCKER_OK"'
```
Expected: `DOCKER_OK`.

- [ ] **Step 2a: If `docker run hello-world` fails** — Docker-in-unprivileged-LXC sometimes needs the host AppArmor/overlay tweak. Fall back to a **privileged** CT: `pct set 122 --features nesting=1,keyctl=1` won't help; recreate with `--unprivileged 0`. (This is the known-good RMNG testbed shape from the docker-port E2E.) Re-run Step 2.

- [ ] **Step 3: Copy the working tree (with all Task 1–5 changes) into the CT**

From the dev machine (this repo root), push over the Proxmox host into the CT's rootfs via rsync-over-ssh through a `pct exec` tar, or the simpler two-hop:
```bash
CTIP=<ip-from-step-1>
# stage a tarball of the repo (excluding target/ and node_modules/) onto the Proxmox host, then into the CT
rsync -az --exclude target --exclude node_modules --exclude frontend/build \
  -e ssh /home/pegasis/Projects/RMNG/ root@10.0.0.100:/tmp/rmng-src/
ssh root@10.0.0.100 'CTID=122; tar -C /tmp/rmng-src -cf - . | pct exec $CTID -- bash -lc "mkdir -p /root/RMNG && tar -C /root/RMNG -xf -"'
```
Expected: `/root/RMNG` populated in the CT (confirm: `pct exec 122 -- ls /root/RMNG/agent-wrapper/agent-instructions.md`).

- [ ] **Step 4: Build the control-server image inside the CT**

```bash
ssh root@10.0.0.100 'CTID=122; pct exec $CTID -- bash -lc "cd /root/RMNG && docker build -t rmng:latest . 2>&1 | tail -20"'
```
Expected: `naming to docker.io/library/rmng:latest` (a successful multi-stage build). This is long (rust + bun + apt). If it OOMs, raise CT memory: `pct set 122 --memory 12288` and retry.

- [ ] **Step 5: Run the control-server container**

```bash
ssh root@10.0.0.100 'CTID=122; pct exec $CTID -- bash -lc "\
  docker rm -f rmng 2>/dev/null; \
  docker run -d --name rmng --privileged --init --pid host --restart unless-stopped \
    -v /var/run/docker.sock:/var/run/docker.sock -v rmng-data:/data -v rmng-sock:/srv/rmng-sock \
    -p 9000-9003:9000-9003 -p 9005:9005 rmng:latest && sleep 8 && \
  curl -fsS localhost:9000/api/config >/dev/null && echo API_UP"'
```
Expected: `API_UP` (the server started and serves the API GPU-free). If the container exits, inspect `docker logs rmng` — if it dies on VA/GPU init at startup (not expected), note it and pivot to running on the W6800 box (CT 106); the file-injection assertion is unchanged.

- [ ] **Step 6: Complete first-run setup + pull the clone template from Hub**

Drive the same endpoints the wizard uses (read `frontend/app/lib/api.ts` + `crates/control-server/src/web.rs` for exact paths; the sequence is: `GET /api/setup/env` → `PUT /api/config` with subnet/hostnamePrefix/monitors → create the `rmng` network + pull `pegasis0/rmng-template:latest` retagged as an `rmng.image=1` source → mark `setupComplete`). Capture the exact calls into a script `scripts/e2e-setup.sh` run inside the CT. Confirm a clone-source image exists:
```bash
ssh root@10.0.0.100 'pct exec 122 -- bash -lc "curl -fsS localhost:9000/api/images | jq -r \".[].reference\""'
```
Expected: at least one `rmng/template:...` reference.

- [ ] **Step 7: Set a DISTINCTIVE playbook (global + a preset append) via the API**

```bash
ssh root@10.0.0.100 'pct exec 122 -- bash -lc "curl -fsS -X PUT localhost:9000/api/config \
  -H \"content-type: application/json\" \
  -d '\''{ \"agentPlaybook\": \"E2E-GLOBAL-MARKER-8811\\n\\nline two\", \"presets\": [{ \"name\": \"e2e\", \"labels\": [], \"linearKey\": \"\", \"vars\": [], \"agentPlaybook\": \"E2E-PRESET-MARKER-4423\" }] }'\'' | jq \".config.agentPlaybook, .config.presets\""'
```
Expected: the response echoes `agentPlaybook` = the global marker and a preset `e2e` with its append.

Confirm persistence to disk:
```bash
ssh root@10.0.0.100 'pct exec 122 -- bash -lc "docker exec rmng cat /data/config.json | jq \".agentPlaybook, .presets[0].agentPlaybook\""'
```
Expected: `"E2E-GLOBAL-MARKER-8811\nline two"` and `"E2E-PRESET-MARKER-4423"`.

- [ ] **Step 8: Create a clone using the `e2e` preset**

Drive `POST /api/clone` (plain mode) with the `e2e` preset (exact body shape from `web.rs` `clone` handler / `frontend/app/lib/api.ts`; roughly `{ "plain": { "title": "e2e", "message": "" }, "image": "<ref from step 6>", "preset": "e2e" }`). Capture the returned op id and poll `GET /api/operations` (or the op endpoint) until the clone container exists:
```bash
ssh root@10.0.0.100 'pct exec 122 -- bash -lc "docker ps --format \"{{.Names}}\" | grep -E \"e2e\" "'
```
Expected: a clone container name is listed (it exists as soon as the `inject` step ran — no need to wait for `done`).

- [ ] **Step 9: THE ASSERTION — the injected file is present with the right owner + composed content**

```bash
ssh root@10.0.0.100 'pct exec 122 -- bash -lc "\
  C=\$(docker ps --format \"{{.Names}}\" | grep e2e | head -1); \
  echo \"clone=\$C\"; \
  docker exec \$C stat -c \"%U:%G %a %n\" /home/rmng/.config/rmng/agent-instructions.md; \
  echo ----; docker exec \$C cat /home/rmng/.config/rmng/agent-instructions.md"'
```
Expected:
- `stat` shows owner `rmng:rmng` (uid/gid 1000), mode `644`.
- The file content is exactly:
  ```
  E2E-GLOBAL-MARKER-8811
  line two

  E2E-PRESET-MARKER-4423
  ```
  (global, blank line, preset append — proving `compose_playbook` + injection end to end.)

- [ ] **Step 10: Validate the wrapper reads THIS path (contract check, no creds)**

```bash
ssh root@10.0.0.100 'pct exec 122 -- bash -lc "\
  C=\$(docker ps --format \"{{.Names}}\" | grep e2e | head -1); \
  docker exec \$C bash -lc \"echo AGENT_INSTRUCTIONS_PATH=\${AGENT_INSTRUCTIONS_PATH:-<default>}; ls -l /home/rmng/.config/rmng/agent-instructions.md\""'
```
Expected: the wrapper's default `AGENT_INSTRUCTIONS_PATH` (`~/.config/rmng/agent-instructions.md`) resolves to the present file — i.e. `resolveSystemAppend` (unit-tested in Task 1) will return the injected content, not the baked-in default. (A live session read is out of scope — no Claude credentials.)

- [ ] **Step 11: Record the result + tear down (optional)**

Append a short results note (CT id, IP, the asserted file content, pass/fail) to the bottom of this plan or a scratch file. Leave the CT running for re-runs, or `ssh root@10.0.0.100 'pct stop 122 && pct destroy 122'` to clean up.

- [ ] **Step 12: Commit any E2E helper scripts**

```bash
git add scripts/e2e-setup.sh   # if created
git commit -m "test(e2e): Proxmox CT validation of injected agent playbook"
```

---

## Self-Review

- **Spec coverage:** §1 merge → Task 1; §2 wrapper read → Task 1; §3 config/wire → Task 2; §4 compose+inject → Task 3; §5 frontend → Task 4; §6 docs → Task 5; testing (wire/provision/wrapper/E2E) → Tasks 1–3 unit tests + Task 6 E2E. All covered.
- **Placeholder scan:** the only intentionally non-verbatim steps are Task 6 Steps 6/8 (wizard + clone API bodies), which depend on live endpoint shapes to be read from `web.rs`/`api.ts` at execution — every assertion command (Steps 7/9/10) is exact.
- **Type consistency:** `agent_playbook` (Rust) / `agentPlaybook` (TS/JSON) used consistently; `compose_playbook(&AppConfig, Option<&Preset>) -> String`; `clone_container(.., agent_playbook: &str, ..)`; `resolveSystemAppend(injectedPath, read?)`; `CloneSpec.agent_playbook: String` (distinct from `agent_instructions: Option<String>`).

---

## E2E result (2026-07-03) — BYTE-EXACT PASS

Ran on a freshly provisioned Proxmox LXC (CT 122 `rmng-playbook-e2e`, Ubuntu 24.04, Docker
29.6.1, at 10.0.0.130) with the feature branch built into `rmng:latest` and run there.

**Validated end-to-end against the running server (my code):**
- Control-server builds + boots GPU-free; first-run setup completes and creates the `rmng` bridge.
- `PUT /api/config` accepts a global `agentPlaybook` **and** a preset `agentPlaybook`, merges them
  (global via `deep_merge`, preset via `merge_presets`), and **persists both to `/data/config.json`**
  verbatim.
- Creating a clone from the `e2e` preset injects `/home/rmng/.config/rmng/agent-instructions.md`
  into the clone with **owner `rmng:rmng` (uid/gid 1000), mode `644`**, and content **byte-exact**
  (`cmp` clean, 55 bytes, sha `597289696d00…`) equal to `compose_playbook` output:
  `E2E-GLOBAL-MARKER-8811\nline two\n\nE2E-PRESET-MARKER-4423` (global + blank line + preset append,
  no trailing newline). The wrapper's default `AGENT_INSTRUCTIONS_PATH` is exactly that path;
  `resolveSystemAppend` (unit-tested) returns the injected content.

**Environment gotcha (not a product bug):** the clone default `docker.cloneCpus` is **16**; a create
on a host/CT with fewer CPUs 400s with `range of CPUs is from 0.01 to 8.00`. For an E2E on a small
CT, first `PUT /api/config {"docker":{"cloneCpus":4,"cloneMemoryMb":6144}}`. (Also required: a GPU
render node is a *runtime* check that fails GPU-less, but clone create/inject do NOT need it — the
video plane does.) Both are orthogonal to the agent-playbook feature.
