# /ng's glass

The surfaces that float over the clone's screen in `/ng` are made of one material: dark glass.
The clone's screen stays the subject; the glass lets its colors show through, blurred, so the
operator keeps a sense of what is under a button or a dialog. What is on the glass is flat.

The classes are in `./glass.css`. The samples are the Storybook story `ng/Design/Glass`.

## The light

There is one light, high above the top of the display. Every glass surface shows it the same way:

| Part | What the light does |
|---|---|
| Rim (1 px edge) | Brightest at the top edge, fades down the sides, a little light on the bottom edge. |
| Face | A sheen on the top part, gone before the middle. |
| Shadow | Falls under the piece. A higher piece casts a longer, softer shadow. |
| Pressed control | Sinks: no sheen, dark at its top inside, 4 % smaller. |

Do not put a light or a shadow anywhere else: no shadow above a piece, no glow from below.

## Glass is the surface only

Glass is the surface of a piece that floats over the clone's screen: a floating button, a
tooltip, a dialog's panel, the monitor's number. What is on a surface (text, icons, fields,
boxes, buttons in a dialog) is flat: solid or translucent fills, 1 px borders, no blur, no rim,
no sheen, no shadow, no glow.

| Class | Use it for | What it is |
|---|---|---|
| `ng-glass` | Every surface. | Dark tint (30 %), blur 8 px, rim, sheen, short shadow, white text. |
| `ng-glass-thick` | A surface with text to read: a dialog, a tooltip, the monitor's number. | Tint 50 %, blur 14 px, a long shadow. |
| `ng-glass-control` | A surface that is itself a control (a floating button). | More light under the mouse; sinks on a press; a white outline on keyboard focus; 45 % when disabled. |
| `ng-glass-lit` | A control whose thing is open (the pages shown, fullscreen on). | Frosted white glass, dark text. |

Join them: `ng-glass ng-glass-control ng-glass-lit` is an "on" button.

## Flat content

| Part | Classes |
|---|---|
| A hollow (the monitors' arrangement) | `rounded-xl border border-white/10 bg-black/25` |
| A field, a select | `rounded-lg border border-white/10 bg-black/30`; focus: `border-white/30` |
| A box (a monitor) | `rounded-lg border border-white/20 bg-neutral-800` |
| A marked box (the primary monitor) | `rounded-lg border border-emerald-400/80 bg-emerald-950` |
| A button | `rounded-lg border border-white/15 bg-white/5`, mouse over: `bg-white/10` |
| The button that ends the task (Done) | `rounded-lg bg-emerald-600`, mouse over: `bg-emerald-500` |
| A small icon button | `rounded-md text-white/60`, mouse over: `bg-white/10 text-white` |

## Shape

- A floating button is round, 44 px.
- A panel's corners: 16 px (`rounded-2xl`). A piece in it: 8 px (`rounded-lg`). A small control
  in a box: 6 px (`rounded-md`).
- The monitor's number: a 144 px square (`size-36`), always the same size, in the top-left
  corner.
- Space between floating pieces, and from the display's edges: 8 px.

## Motion

- A control's light and press: 150 ms, ease out.
- A piece that comes in or goes out (the floating buttons, the pages): 300 ms, ease out, along
  the way it goes (to the right for the buttons).
- A dialog: in from a blur, 200 ms (`~/components/ModalShell.tsx`).
- A tooltip: comes after 300 ms of the mouse at rest, fades in.

## Rules for the code

- Do not put `shadow-*` or `ring-*` utilities on glass: they replace its whole `box-shadow`
  (its rim shade and its shadow). Change a custom property instead, for example
  `[--glass-tint:rgb(23_23_23/0.9)]`.
- Glass blurs only what is under it inside its nearest "backdrop root": an ancestor with a
  `backdrop-filter`, a `filter`, an `opacity` under 1, or a `mask`. So a glass surface is never
  in another one: a tooltip of a glass button is its sibling, not its child.
- Blur costs work on each frame of the video under it. Use it on the floating surfaces only.
- The custom properties: `--glass-tint`, `--glass-blur`, `--glass-sheen`, `--glass-over` (a
  layer on the tint: the mouse's light, a press's shade), `--glass-rim-top`,
  `--glass-rim-side`, `--glass-rim-bottom`, `--glass-inner`, `--glass-drop`.
