# frontend

The web app — the **management UI** (port 2). It is the existing React Router 7 dashboard
(`../../control-server/app`) kept and extended with a Settings page, built to a static
bundle and **served by the Rust control-server**.

It is **management only**: host selection, clone orchestration, Claude accounts, chat,
notes, and settings. **The live monitor viewer is not here** — viewing is the native GTK
[viewer](../crates/viewer/README.md) on port 1. There is **no `MonitorViewer`, no WebRTC,
no signaling** in this app.

## What's reused (≈99% of the current UI)

- Sidebar host list + selection, drag-reorder (`SidebarHost`, dnd-kit).
- `CloneModal` (existing/new/plain ticket + Claude account picker).
- `ClaudeAccountsPanel` (5h/7d usage bars).
- `ChatPanel` (per-host agent chat over `/api/chat/:id/events`).
- `HostEditor` (notes/BlockNote + uploads).
- `OperationProgress` (clone/delete progress).
- SSE subscription to `/events` → `ControlState`.

The two-pane layout stays Notes/Chat (`pane: "notes" | "chat"`) — the planned third
"monitor" pane is dropped.

## What's new

- **`Settings.tsx`** — per-section forms for **all** configuration (the Docker backend —
  daemon socket / `rmng`-network subnet / hostname prefix / per-clone limits, presets —
  Linear key + auto-select ticket-id prefixes + env vars, Claude polling/groups, monitor defaults, the
  four listen ports), replacing hand-edited `config.json`. Secret fields are
  masked/write-only with **Test connection** buttons (`POST /api/config/test`); saves go to
  `PUT /api/config` and apply live. Claude accounts are imported from a signed-in clone
  (`ClaudeAccountsPanel`), not entered here. Reads the redacted `GET /api/config` —
  plaintext secrets never reach the browser.
- A **"+ Pull template"** affordance (`POST /api/images/pull` — prompts for a registry
  reference, prefilled with the configured `docker.templateReference`), plus commit-a-clone
  (`POST /api/images/commit`), with progress shown via the existing `OperationProgress`.
- **Image picker** in `CloneModal`: pick a clone-source image (from `GET /api/images`) to
  clone from — the clone streams as an `Operation`.
- **Claude account controls** (extending `ClaudeAccountsPanel` + the per-host card): show the
  assigned account, **hot-swap** a running clone's token to another account (`POST
  /api/claude/swap`). Usage bars drive the recommendation; "auto" clones rotate across
  all imported accounts as they exhaust.

## Types

TypeScript types are **generated from the `wire` crate via ts-rs** (replacing the
hand-maintained `app/lib/types.ts`), so `ControlState`/`Host`/`Operation`/etc. cannot drift
from the Rust backend.

## Build & serve

- Build with Vite/Bun to a static bundle (`frontend/build` or similar).
- The Rust control-server serves the bundle + `/uploads` on port 2; no Bun runtime in
  production.
- Dev: Vite dev server proxying `/api` + `/events` to the Rust backend.

## Backend contract (preserved by the Rust port)

- `GET /events` → full `ControlState` JSON, `data: …\n\n` frames, `: ping` heartbeats.
- `POST /api/{activate,reorder,clone,delete}`, `/api/claude/{import,refresh,recommended}`,
  `/api/notes/:id`, `/api/upload`, `/api/chat/:id` (+ `/events`, `/abort`).
- **New**: `/api/config` (GET/PUT), `/api/config/test`, `/api/setup/env`, `/api/images/*`,
  `/api/claude/swap` (hot-swap a clone's token); `/api/clone` takes an `image` reference.

## Tests

- Type-check against generated `wire` types.
- Selection + clone/chat/claude flows work against the Rust backend (parity with Bun).
- Settings: redacted read, write-only secrets, live-apply, "Test connection".

## Open questions

- Whether to keep React Router 7 + Bun build or simplify to a plain Vite SPA (the backend
  serves it either way).
