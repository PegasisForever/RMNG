# Editable agent playbook — design

**Date:** 2026-07-03
**Status:** Approved (brainstorm), pending implementation plan
**Touches:** `agent-wrapper` (instruction source + delivery), `crates/wire` (config), `crates/control-server` (seed + inject), `frontend` (Settings UI).

## Summary

Make the desktop agent's baked-in instructions — today the two files
[`agent-wrapper/operating-notes.md`](../../../agent-wrapper/operating-notes.md) and
[`agent-wrapper/ticket-procedure.md`](../../../agent-wrapper/ticket-procedure.md), compiled
into the wrapper binary and joined into the system-prompt `append` (`SYSTEM_APPEND`) —
**editable from the control-server Settings UI**, applied to the **next** clone created.

The two files merge into one canonical `agent-wrapper/agent-instructions.md`. A new global
setting (`agentPlaybook`) holds that text, seeded with the shipped default and editable in
the UI. An optional per-preset field appends preset-specific text after the global text. At
clone creation the composed text is injected as a file into the clone; the wrapper reads that
file (falling back to its baked-in default when absent). Existing clones are unaffected — the
file is written when a clone is created and read once at session start.

## Requirements (locked during brainstorm)

| Decision | Choice |
|---|---|
| Scope | **Global setting shared by all presets**, plus an **optional per-preset field** |
| Composition | Per-preset text **appends after** the global text (empty preset field ⇒ global only) |
| Default handling | Global field **seeds the full combined default text** into config (WYSIWYG; edited directly) |
| Source of truth | The two `.md` files **merge into one** `agent-wrapper/agent-instructions.md`, embedded by both binaries |
| Naming | New fields are named **`agent_playbook`** to avoid overlap with the existing per-clone `agent_instructions`/`claude_instructions` (left untouched) |
| Apply timing | **Next clone only** — injected at creation; no control-server restart; existing clones unchanged |

**Explicitly out of scope (YAGNI):** hot-reloading the playbook into running clones; a
per-clone one-off playbook override (the existing per-clone `agentInstructions` one-off, folded
into the first task message, already covers task-specific additions); versioning/history of
edits; separate editing of the "notes" vs "procedure" halves (one text blob).

## Background: the three instruction layers

This feature adds the **base** layer. The other two already exist and are **not changed**.

| Layer | Content | Injection point | Lifetime | Status |
|---|---|---|---|---|
| **Base playbook** | operating notes + ticket procedure | wrapper system-prompt `append` (`SYSTEM_APPEND`) | persistent per session | **new** |
| **Preset append** | preset-specific additions | system-prompt `append`, after the base | persistent per session | **new** |
| Per-clone one-off | "Additional host-agent / Claude Code instructions" from CloneModal | folded into the **first task message** ([`chat.rs:337`](../../../crates/control-server/src/chat.rs#L337)) | one task | exists, untouched |

Effective wrapper `append` = **global `agentPlaybook`** `+ "\n\n" +` **preset `agentPlaybook`**
(the join is skipped when the preset field is empty).

## Architecture

```
Settings UI (textarea)                     config.json
  agentPlaybook (global) ───PUT /api/config──▶ AppConfig.agent_playbook   (seeded default)
  preset.agentPlaybook  ─────────────────────▶ Preset.agent_playbook      (optional, empty ok)
                                                        │
                                    clone create: compose = global + "\n\n" + preset (if any)
                                                        │
                                                        ▼  (provision.rs upload_tar)
                          clone:/home/rmng/.config/rmng/agent-instructions.md
                                                        │
                                                        ▼  (agent-wrapper reads at startup)
                    SYSTEM_APPEND = <injected file> ?? <baked-in agent-instructions.md>
                                                        │
                                                        ▼
                             query({ systemPrompt: { preset: claude_code, append } })
```

## Detailed changes

### 1. Merge the instruction files (`agent-wrapper/`)

- Create `agent-wrapper/agent-instructions.md` = current `operating-notes.md` body, then a blank
  line, then `ticket-procedure.md` body (exactly today's `SYSTEM_APPEND` order and separator).
- Delete `operating-notes.md` and `ticket-procedure.md`.
- [`server.ts`](../../../agent-wrapper/src/server.ts): replace the two
  `with { type: "text" }` imports ([lines 38–39](../../../agent-wrapper/src/server.ts#L38-L39))
  and the `SYSTEM_APPEND` join ([line 56](../../../agent-wrapper/src/server.ts#L56)) with a
  single import of `agent-instructions.md` used as the **baked-in default**.

### 2. Wrapper reads the injected file (`agent-wrapper/`)

- [`config.ts`](../../../agent-wrapper/src/config.ts): add
  `instructionsPath` (env `AGENT_INSTRUCTIONS_PATH`, default
  `${HOME}/.config/rmng/agent-instructions.md`).
- [`server.ts`](../../../agent-wrapper/src/server.ts): at startup,
  `SYSTEM_APPEND = readIfPresentNonEmpty(CONFIG.instructionsPath) ?? BAKED_IN_DEFAULT`.
  A non-empty injected file wins; absent/empty/unreadable (local `bun run` dev, robustness) ⇒
  the baked-in default. Read once at process start (matches the existing "session id in memory
  only" model — a fresh clone boots a fresh wrapper).

### 3. Config / wire (`crates/wire/src/config.rs`)

- `AppConfig.agent_playbook: String`, `#[serde(default = "default_agent_playbook")]`, where
  `default_agent_playbook()` returns
  `include_str!("../../../agent-wrapper/agent-instructions.md").to_string()` — the **same** file
  the wrapper embeds, so a config missing the key shows the shipped default in the UI and it is
  persisted to `config.json` on the first save. Non-secret ⇒ passes through into
  `AppConfigRedacted.agent_playbook`. (Not yet deployed, so there is no existing-config migration
  to consider — the serde default is purely a clean way to surface the shipped text as the
  editable value.)
- `Preset.agent_playbook: String`, `#[serde(default)]` (empty ⇒ no append). Non-secret ⇒ passes
  through into `PresetRedacted.agent_playbook` (alongside `vars`/`labels`).
- `ConfigPutResponse` / restart-required: **not** restart-required (read fresh per clone, like
  `detector_inference_url`).
- Note on the cross-crate `include_str!`: it couples the control-server build to
  `agent-wrapper/agent-instructions.md`. Accepted: it is the single source of truth for the
  default text (the alternative — a second copy — drifts). If the path is unavailable in some
  build context, this is a compile-time error, surfaced immediately.
- Merge logic ([`crates/control-server/src/config.rs`](../../../crates/control-server/src/config.rs))
  accepts both new fields on `PUT /api/config`.

### 4. Compose + inject at clone creation (`crates/control-server/`)

- Compose the effective text where the preset is resolved
  ([`web.rs:336`/`379`](../../../crates/control-server/src/web.rs#L379), the `preset_env`
  call sites): `agent_playbook = cfg.agent_playbook` + (if preset field non-empty)
  `"\n\n" + preset.agent_playbook`.
- Thread it through as a **new** `CloneSpec.agent_playbook: String`
  ([`jobs.rs:49`](../../../crates/control-server/src/jobs.rs#L49)) — distinct from the existing
  `CloneSpec.agent_instructions: Option<String>` (the per-clone one-off; unchanged) — and on to
  `clone_container` / `clone_container_after_create`.
- In [`clone_container_after_create`](../../../crates/control-server/src/provision.rs#L317),
  add a `TarEntry` to the existing `upload_tar` batch ([provision.rs:335](../../../crates/control-server/src/provision.rs#L335)):
  ```
  path: home/{CLONE_USER}/.config/rmng/agent-instructions.md
  mode: 0o644, uid: CLONE_UID, gid: CLONE_GID   // = 1000/1000
  data: composed agent_playbook (skip the entry if the composed string is empty)
  ```

### 5. Frontend ([`SettingsPanel.tsx`](../../../frontend/app/components/SettingsPanel.tsx))

- New global `<Section title="Agent instructions">` (hint: "The desktop agent's operating notes
  + ticket procedure, injected as its system prompt. Applies to newly created clones."), a
  full-width `<textarea>` bound to new state `agentPlaybook`, seeded from `c.agentPlaybook`
  ([init near line 250](../../../frontend/app/components/SettingsPanel.tsx#L250)), included in the
  `PUT` payload ([near line 348](../../../frontend/app/components/SettingsPanel.tsx#L348)).
- Per-preset `<textarea>` in the existing preset editor
  ([line 483](../../../frontend/app/components/SettingsPanel.tsx#L483)) — label "Extra agent
  instructions (appended after the global instructions for this preset)"; added to the local
  preset state ([line 199](../../../frontend/app/components/SettingsPanel.tsx#L199)) and the
  presets payload ([line 349](../../../frontend/app/components/SettingsPanel.tsx#L349)).
- `ts-rs` regenerates `frontend/app/lib/wire/AppConfigRedacted.ts` and `PresetRedacted.ts`.

## Testing

- **wire** (`config.rs` tests): missing `agentPlaybook` ⇒ default text non-empty and equal to
  the embedded file; preset `agentPlaybook` defaults to `""`; redaction passes both fields
  through unchanged; camelCase round-trip.
- **provision** (`provision.rs` tests): composition — global only; global + non-empty preset
  (joined with one blank line, correct order); empty preset ⇒ global only; empty composed ⇒ no
  tar entry. Assert the tar entry path/owner/mode when present.
- **agent-wrapper**: injected non-empty file wins; absent file ⇒ baked-in default; empty file ⇒
  baked-in default.
- **manual/E2E**: edit the global text in Settings, create a clone, confirm
  `~/.config/rmng/agent-instructions.md` in the clone matches, and the agent's behavior reflects
  it; set a preset append and confirm it lands after the global text for a clone of that preset.

## Docs to update

- [`agent-wrapper/README.md`](../../../agent-wrapper/README.md) — the "instructions come in two
  layers" section: the baked-in notes/procedure are now one editable file, overridable per clone
  by the injected `agent-instructions.md`; document `AGENT_INSTRUCTIONS_PATH`.
- `docs/` config/protocol references that enumerate `AppConfig` / preset fields
  (e.g. `docs/PROTOCOL.md`, `docs/API.md`) gain `agentPlaybook` (global + preset).
