# frontend

The web app (port 9000): a React Router 7 single-page app, built to a static bundle and
**served by the Rust control-server**. It shows the selected clone's screen and, over it, the
control panel: account usage, tickets, clones, notes and settings. The same app runs on a
desktop (mouse and keyboard, one browser window per monitor) and on a phone or a tablet
(touch, an on-screen keyboard, Add to Home Screen).

## Pages and addresses

- `/`: the shell (`public/shell.html`, sent by the server). It holds the app in a frame and
  keeps the fullscreen and the key lock, so a new build reloads only the frame. A browser with
  no fullscreen (an iPhone) goes straight to the app.
- `/app`: the app (`app/routes/app.tsx`). A server whose first-run setup is not done shows the
  setup wizard here.
- `/screen-area`: a test page of the screen's areas, opened from Settings.

The address holds what is open, so Back works and a copied address shows the same:
`?clone=<id>` (the open clone), `?ticket=<id>` (the open ticket, `app/lib/selection.ts`),
`?monitor=<N>` (this window's monitor of the browser's layout, 0 when absent) and
`?control-panel` (only the control panel, in a window of its own). The shell's address is the
app's: `/?clone=…`, `/?monitor=1`.

## Where things are

- `app/desk/`: the screen (`FullDesk.tsx`, `ScreenView.tsx`), its floating buttons, the
  on-screen keyboard and trackpad, the terminal of a headless clone, and the connection to the
  selected clone (`remote.ts`, `session.ts`). What the keys' and the trackpad's touches do is
  `touchInput.ts`, with no React; it writes to a clone sink (`cloneSink.ts`: the desktop's
  session, or the terminal). The glass look is in `app/desk/GLASS.md`.
- `app/desk/control/`: the control panel's tabs (Usage, Tickets, Clones, Notes, Settings).
- `app/viewer/`: the media client. A worker opens the WebSocket (`/api/viewer`), decodes the
  monitor's H.264 with WebCodecs and draws it. Monitor 0's window also plays the selected
  clone's sound (Opus, decoded in the worker, played by an AudioWorklet: `sound*.ts`). Also the
  input, the clipboard, and file upload. The protocol is in `../docs/PROTOCOL.md`.
- `app/components/`: dialogs (New clone, tickets, accounts, layout, port forward), the Settings
  panes, chat and notes.
- `app/lib/`: the live state over SSE (`useLiveState.ts`: `/events` → `ControlState`), the
  API calls (`api.ts`), the monitor layouts (`clientLayout.ts`), and the generated wire types.
- `public/`: the shell, the web app manifest and the service worker.

## Types

TypeScript types are **generated from the `wire` crate via ts-rs** into `app/lib/wire/`
(`cargo test -p wire` writes them), so `ControlState`, `Operation` and the rest cannot drift
from the Rust backend.

## Build & serve

- `bun run build` builds the static bundle to `frontend/build/client`.
- The Rust control-server serves the bundle and `/uploads` on port 9000; there is no Bun
  runtime in production.
- Dev: `bun run dev` starts the Vite dev server on port 9000 with no proxy, so the API and
  the clone's screen do not work there. To test them, run `bun run build`, then
  `cargo run -p control-server`, which serves `frontend/build/client`.
- The app needs HTTPS (or `localhost`) for WebCodecs, the async clipboard and Keyboard Lock.
- `bun test` runs the unit tests; `bun run storybook` shows the stories.

The API is in `../docs/API.md`.
