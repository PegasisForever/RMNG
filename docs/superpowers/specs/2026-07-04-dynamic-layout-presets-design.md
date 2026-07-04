# Dynamic monitor layouts & layout presets — design

**Status:** approved design, ready for implementation planning
**Date:** 2026-07-04

## Goal

Let an operator store multiple named **layout presets** (each a full monitor
arrangement) in Settings and switch the active one from the web sidebar. Switching
reconfigures every running clone's monitors **live — without closing any running
programs** — and every attached [viewer](../../../crates/viewer/README.md) reflows to
the new arrangement immediately.

Today a single server-global layout (`AppConfig.monitors`,
[config.rs:300](../../../crates/wire/src/config.rs#L300)) is baked into each clone at
provision and can only be changed on a running clone by
[`apply-monitors.sh`](../../../crates/control-server/scripts/apply-monitors.sh), which
**restarts gnome-headless and thus kills every app**. This feature replaces that with
named presets, a sidebar switcher, and true live reconfiguration.

## Decisions (locked)

- **Fleet-wide scope.** Activating a preset applies to **all running clones** at once
  (a single fleet-wide active layout, like today's global `monitors` — just selectable
  by name and applied live). Per-clone independent layouts are explicitly **not** in
  scope.
- **Live reconfiguration, no session restart (Approach A).** The clone-daemon diffs
  the desired monitor set against its current `RecordVirtual` streams and adds / stops /
  recreates individual streams on its **already-running** Mutter session. gnome-shell is
  never restarted, so apps stay open. Only monitors that actually changed churn; a
  monitor with an unchanged `WxH` keeps streaming without a blip. **No fallback path** —
  the design commits to A (validated first by a spike; see Testing).
- **Server is the source of truth for the active layout.** The control-server pushes
  the active layout to each daemon over the existing clone unix socket — on the daemon's
  `Hello` and on every activation. Baked `RMNG_MONITORS` becomes only a pre-connect boot
  default.
- **Presets live in `config.json`, non-secret.** New `AppConfig.layout_presets` +
  `AppConfig.active_layout`. Edited through the existing `PUT /api/config` merge;
  activated through a new `POST /api/layout/activate`.
- **Segmented-button sidebar switcher.** One pill per preset, active one highlighted.
- **The restart-based path is deleted.** `apply-monitors.sh`, `provision::apply_monitors`,
  and `POST /api/monitors/apply` are removed — they violate the no-app-loss rule and are
  superseded.
- **Naming.** The existing clone-provisioning `presets`
  ([config.rs](../../../crates/wire/src/config.rs), env/Linear) are untouched and keep
  the name "clone presets." The new ones are "layout presets" everywhere
  (`layout_presets`, `/api/layout/activate`) to avoid collision.

## Why this shape

- **Fleet-wide keeps the encoder set correct as-is.** One global active layout means the
  control-server's per-monitor encoder map keyed on `(monitor_id, w, h)` stays a
  fan-out/rebuild problem, not a re-architecture. Per-clone layouts would fork the
  encoder set per clone and require a new per-`Host` monitor field.
- **Approach A is the only one that meets "no blip on unchanged monitors."** The two
  rejected mechanisms both fail a hard requirement or add cost:
  - **Rebuild the daemon's whole Mutter session (Approach C).** Simpler code, still no
    gnome-shell restart, but every monitor drops for ~1–2 s and Mutter reshuffles all
    windows when all outputs briefly vanish. Rejected for UX.
  - **Fixed pool of pre-created monitors + `ApplyMonitorsConfig` only (Approach B).**
    Forces every resolution any preset uses to be declared at gnome-shell boot
    (`MUTTER_DEBUG_DUMMY_MODE_SPECS`), caps the monitor count, and awkwardly couples
    capture streams to connectors. Rejected.
- **Approach A has strong precedent.** `gnome-remote-desktop` uses these exact private
  Mutter APIs (`ScreenCast.RecordVirtual` + `RemoteDesktop` + `DisplayConfig.ApplyMonitorsConfig`)
  and adds/removes/resizes virtual monitors live for dynamic RDP resolution changes.
- **Server-as-source-of-truth removes env-rewriting.** Because the daemon receives the
  active layout on `Hello`, we never `sed` systemd units or restart anything for
  persistence; `RMNG_MONITORS` is just a bootstrap default that the socket handshake
  corrects.

## Architecture

### 1. Data model & config

[`crates/wire/src/config.rs`](../../../crates/wire/src/config.rs):

```rust
pub struct LayoutPreset {
    pub name: String,
    pub monitors: Vec<MonitorSpec>,   // existing { width, height, x, y, primary }
}
// AppConfig gains:
pub layout_presets: Vec<LayoutPreset>,
pub active_layout: String,            // name of the active preset
```

- `LayoutPreset` is non-secret → passes through `AppConfigRedacted`
  ([config.rs:421+](../../../crates/wire/src/config.rs#L421)) whole; new ts-rs export
  `frontend/app/lib/wire/LayoutPreset.ts`.
- `effective_monitors()`
  ([config.rs:385](../../../crates/wire/src/config.rs#L385)) resolves to the **active
  preset's** monitors → fallback to the first preset → fallback to the dual-1440p
  default.
- **Load migration (one-shot):** if `layout_presets` is empty, seed
  `[{ name: "Default", monitors: <legacy `monitors` or dual-1440p> }]` and set
  `active_layout = "Default"`. Serde keeps *reading* the legacy `monitors` field for
  this migration only; it is no longer written.
- New clones bake the active layout automatically — `provision::monitors_csv()`
  ([provision.rs:50](../../../crates/control-server/src/provision.rs#L50)) already
  derives from `effective_monitors()`.

### 2. Control plane — API & fleet-wide apply

- **Preset CRUD rides on `PUT /api/config`** (existing merge, same as clone presets):
  add / rename / edit / remove entries in `layout_presets`. No new CRUD endpoints.
- **`POST /api/layout/activate { name }`** ([web.rs](../../../crates/control-server/src/web.rs)):
  1. Validate `name` exists (unknown → `400`).
  2. Set `active_layout`, persist config (→ SSE broadcast; see §5).
  3. For each **running** clone, send `ServerMsg::SetMonitors { monitors }` over the
     clone socket. Best-effort per clone (§6).
- **`ServerMsg::SetMonitors`** — new variant in
  [`socket.rs`](../../../crates/wire/src/socket.rs) `ServerMsg` (serde tag `t`),
  payload `{ monitors: Vec<MonitorSpec> }`.
- The mediaplane sends `SetMonitors` to a daemon **on `Hello`** as well
  ([mediaplane.rs](../../../crates/control-server/src/mediaplane.rs) subscribe/prime
  path), so a reconnecting clone with a stale baked layout is corrected.

### 3. Clone-daemon — live reconfiguration (Approach A)

[`crates/clone-daemon/src/mutter.rs`](../../../crates/clone-daemon/src/mutter.rs) +
[`main.rs`](../../../crates/clone-daemon/src/main.rs):

- **Make `Session` reconfigurable.** Store the `ScreenCastSession` proxy + path and each
  per-stream `Stream` proxy in `Session` (today `setup_with_cursor_mode` drops the
  ScreenCast proxy after setup, [mutter.rs:159](../../../crates/clone-daemon/src/mutter.rs#L159)).
  Add a `Stop()` method to the `org.gnome.Mutter.ScreenCast.Stream` zbus proxy
  ([mutter.rs:103](../../../crates/clone-daemon/src/mutter.rs#L103)).
- **`Session::reconfigure(desired: &[MonitorCfg])` — minimal diff:**
  - Greedy-match each desired monitor to an existing stream of the **same `WxH`** →
    reuse it untouched (no blip).
  - Desired monitors with no match → `RecordVirtual` a new stream on the live session.
  - Leftover unmatched streams → `Stop`.
  - A resized monitor is inherently stop-old + start-new (RecordVirtual's mode is fixed
    at creation) — only that monitor churns.
- **Stable identity.** `monitor_id` = **logical slot index** in the active layout (not
  creation order), so a resized monitor keeps its id (hence its viewer window) while its
  PipeWire node id changes underneath. The daemon maps node_id → slot for frame tagging.
- **Connectors after churn.** `apply_layout`'s `Meta-<i>`-by-creation-order assumption
  ([main.rs:238-253](../../../crates/clone-daemon/src/main.rs#L238-L253)) only holds for
  the initial all-at-once build. After a diff, re-read `DisplayConfig.GetCurrentState`
  to learn each virtual monitor's real connector name, then `ApplyMonitorsConfig` with
  those names + the new `WxH@60.000` mode ids to set positions/primary.
- **Plumbing.** The socket handler gains `ServerMsg::SetMonitors` → `reconfigure` → the
  capture loop starts/stops per-node pipelines for the changed nodes → reply
  `DaemonMsg::Layout { monitors }` (existing, [socket.rs:148](../../../crates/wire/src/socket.rs#L148)).
  Reconfigure is serialized against capture via the session lock; capture tasks keyed by
  node id handle add/remove.

### 4. Server media plane & viewer reflow

**Server** ([mediaplane.rs](../../../crates/control-server/src/mediaplane.rs)): already
stores + rebroadcasts `DaemonMsg::Layout` and rebuilds a monitor's encoder on
`(monitor_id, w, h)` change ([~515-525](../../../crates/control-server/src/mediaplane.rs#L515-L525)).
Extend to **add** encoders for new ids and **drop** them for removed ids, then push the
`T_LAYOUT` (tag-3) message to that clone's viewers — the same code path used on
selection change, now also fired on live layout change.

**Viewer** ([viewer/src/main.rs](../../../crates/viewer/src/main.rs)) — three additions,
all following the existing tag-5-forwards *reconcile* pattern
([main.rs:290-301](../../../crates/viewer/src/main.rs#L290-L301)):

1. **On tag-3 layout, reconcile the window set:** build windows for new `monitor_id`s,
   **destroy windows for ids no longer present** (today it only ever adds,
   [main.rs:509-557](../../../crates/viewer/src/main.rs#L509-L557)), reposition/resize
   existing ones per the new `MonitorPlacement`.
2. **Resolution change on a surviving id:** tear down + rebuild just that monitor's
   decode pipeline (appsrc/decoder) so it renegotiates caps to the new size — reuses the
   existing lazy-build path; the encoder's IDR at the new size drives the rebuild.
3. **Preserve the "main window" invariant:** the close/Settings headerbar
   ([main.rs:509-532](../../../crates/viewer/src/main.rs#L509-L532)) must survive a
   reconfigure. Re-designate the main window to the current primary/lowest id rather than
   letting it be torn down, so the operator never loses those controls.

Cross-window drag routing already refreshes from tag-3
([main.rs:546](../../../crates/viewer/src/main.rs#L546)), so it just works.

### 5. Frontend

**Settings → "Layout presets"** (replaces the single Monitors section in
[SettingsPanel.tsx](../../../frontend/app/components/SettingsPanel.tsx)):

- A list of named presets; each card = editable **name** + the existing
  [`MonitorsEditor`](../../../frontend/app/components/MonitorsEditor.tsx) bound to its
  `monitors` + a remove button. Plus "Add preset."
- Saved via the existing `PUT /api/config` merge.
- The **"Apply to running clones"** button is removed from `MonitorsEditor`
  ([MonitorsEditor.tsx:155-169](../../../frontend/app/components/MonitorsEditor.tsx#L155-L169))
  — that was the restart path. `MonitorsEditor` is otherwise reused unchanged (already a
  controlled `monitors`-in / `onChange`-out component).

**Sidebar switcher** ([Sidebar.tsx](../../../frontend/app/components/Sidebar.tsx)):

- **Segmented buttons** — one pill per preset, active highlighted. Selecting → `POST
  /api/layout/activate { name }`.
- **Live cross-browser reactivity:** mirror `active_layout` + the preset name list into
  `ControlState` ([control.rs](../../../crates/wire/src/control.rs)) so the switcher
  updates over the existing `/events` SSE
  ([_index.tsx:70-90](../../../frontend/app/routes/_index.tsx#L70-L90)) — no separate
  config fetch, and every operator's sidebar stays in sync when anyone switches.

## Data flow (after)

```
Settings edit ──PUT /api/config──► layout_presets merged, persisted
Sidebar pill  ──POST /api/layout/activate {name}──► active_layout set, persisted
                                                     │
              ┌──────────────────────────────────────┤ ServerMsg::SetMonitors per running clone
              ▼                                        ▼
        clone-daemon A                           clone-daemon B
   Session::reconfigure(diff)               Session::reconfigure(diff)
   add/stop/recreate RecordVirtual          (apps stay open)
   ApplyMonitorsConfig(new positions)
              │ DaemonMsg::Layout{monitors}
              ▼
        control-server: rebuild/add/drop per-monitor encoders
              │ T_LAYOUT (tag 3) to that clone's viewers
              ▼
        viewer: reconcile windows (add/remove/resize), rebuild changed pipelines
```

## Scope

- **In scope:** `wire` (`config` — `LayoutPreset`, `layout_presets`, `active_layout`,
  migration; `socket` — `ServerMsg::SetMonitors`; `control` — mirror active layout into
  `ControlState`), `control-server` (`web.rs` activate endpoint, `mediaplane.rs`
  SetMonitors dispatch on activate + Hello, encoder add/drop on layout change),
  `clone-daemon` (`mutter.rs` reconfigurable `Session` + `Stream.Stop`, `main.rs`
  `reconfigure` diff + connector remap + SetMonitors handler), `viewer` (`main.rs`
  window reconcile + pipeline rebuild + main-window preservation), `frontend`
  (`SettingsPanel` layout-presets editor, `Sidebar` segmented switcher, new wire type).
- **Removed:** `apply-monitors.sh`, `provision::apply_monitors`, `POST
  /api/monitors/apply`, the `AppConfig.monitors` write path.
- **Out of scope / unchanged:** clone `presets` (env/Linear), per-clone independent
  layouts, refresh rates other than 60 Hz, chroma handshake.

## Error handling & edge cases

- **Preset validation:** ≥1 monitor and exactly one primary per preset (MonitorsEditor
  enforces primary; add the min-1 guard); unique, non-empty names. Can't delete the last
  preset. Deleting the **active** preset re-points `active_layout` to the first remaining
  and applies it. Renaming the active preset updates the `active_layout` pointer.
- **Fleet apply is best-effort, per-clone.** The endpoint persists config and returns
  after dispatching; one clone failing to reconfigure is logged/surfaced per-clone and
  does not block others.
- **A daemon that can't apply** (e.g. `RecordVirtual` error) keeps its current layout and
  reports failure; its viewers stay on the last-good layout — never a half-broken state.
- **Concurrent activates** → last-writer-wins on config; each dispatch is idempotent per
  daemon (a diff that matches current is a no-op).
- **No clones running** → activate just persists; applies on next boot (new clones bake
  it; reconnecting clones get it on `Hello`).
- **Clone connects mid-switch** → receives the active layout on `Hello`, reconfigures if
  it differs from its baked default.

## Testing

- **Feasibility spike FIRST** (CT 106 / W6800, the only VA-API encode box): open real
  apps in a clone, then live add / remove / resize a `RecordVirtual` stream on the
  running Mutter session and confirm apps stay open. This validates Approach A before the
  rest is built.
- **Unit:** config migration (legacy `monitors` → "Default" preset), `effective_monitors()`
  resolution, `monitors_csv()` from active preset, preset-CRUD merge, and the
  `reconfigure` **diff** as a pure function (desired vs current → keep / add / stop sets).
- **Integration:** control-client drives `activate`; assert `DaemonMsg::Layout` reflects
  the new set and encoders rebuild.
- **Manual E2E on CT 106:** open apps, switch presets from the sidebar, confirm apps stay
  open and the viewer reflows for all four change types (add, remove, resize, reposition);
  verify the two-viewer case (both reflow).
