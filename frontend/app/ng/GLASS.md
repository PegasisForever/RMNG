# /ng's glass

The surfaces that float over the clone's screen in `/ng` are made of one material: glass. The
clone's screen stays the subject; the glass lets its colors show through, blurred, so the
operator keeps a sense of what is under a button or a dialog. What is on the glass is flat.

Glass is light or dark as the OS says (`/ng` sets no theme of its own; Storybook: the Theme in
the toolbar). Light glass is white with dark text; dark glass is grey with light text.

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
tooltip, a dialog's panel, the monitor's number, a note over the screen ("Connecting…", the
words dictation hears), the keyboard's panel. What is on a surface (text, icons, fields,
boxes, buttons in a dialog) is flat: solid or translucent fills, 1 px borders, no blur, no rim,
no sheen, no shadow, no glow. One exception: the button that asks the operator to repair a
card (an account's "Token expired. Sign in again") is a glass control
(`ng-glass ng-glass-control rounded-lg`), with red text (`text-rose-600` / `text-rose-400`), so
it stands out of the flat card as a thing to press.
The assistant's chat bubbles are glass too (`ng-glass`; the operator's own in the other look,
`ng-glass ng-glass-lit`), as messages that float in the panel; its field and buttons are the
flat ones below, translucent, not solid. The side panels, the notes, the chat and an open
ticket scroll with no bar showing (`NO_SCROLLBARS`, `./control/ui.tsx`).
Settings in the control panel is the dashboard's own panes, drawn in slate. Under `.ng-on-glass`
(`./glass.css`) their slate shades become this flat content: in light, the light shades (fills,
lines) are black at a low opacity and the text shades are greys dark enough to read, and a white
fill is see-through white; in dark, the text shades are white at a high opacity and the fills and
lines are white or black at a low one. No solid white or blue box is left.

| Class | Use it for | Light | Dark |
|---|---|---|---|
| `ng-glass` | Every surface: rim, sheen, short shadow. Blur 8 px. | White tint 35 %, dark text, a hairline edge. | Grey tint 30 %, white text. |
| `ng-glass-thick` | A surface with text to read: a dialog, a tooltip, the monitor's number, a note. Blur 14 px, a long shadow. | White tint 55 %. | Grey tint 50 %. |
| `ng-glass-control` | A surface that is itself a control (a floating button): more light under the mouse; sinks on a press; an outline on keyboard focus; 45 % when disabled. | Dark outline. | White outline. |
| `ng-glass-flat` | A piece on a backdrop that already blurs and darkens the screen (the wide control panel's columns): no blur and no shadow of its own, only the tint, the rim and the sheen. | | |
| `ng-glass-lit` | A control whose thing is open (the control panel shown, fullscreen on): the other look's glass. | Dark glass, white text. | Frosted white glass, dark text. |

Join them: `ng-glass ng-glass-control ng-glass-lit` is an "on" button.

The control panel is flat glass (`ng-glass-flat`) on a shared backdrop: narrow, its one panel
(`./control/ui.tsx`, `CONTROL_PANEL_GLASS`); wide, each column, with no panel round them. Each side panel
over the columns is the usual thick glass, with its own blur and long shadow, as it floats over
them. With a mouse on a large screen, the side panels also slide in over the clone's screen
without the control panel (the mouse at the right edge): the same thick glass, with no backdrop. While the control panel is open, a backdrop over the whole screen (under the control panel and the
floating buttons) blurs and darkens it (`CONTROL_PANEL_BACKDROP`, `bg-black/25` / `bg-black/40`, blur
12 px); it fades in and out with the control panel (200 ms), and takes no touch or click. In the control panel's own window there is no clone's
screen under them: the same flat content sits on a solid ground (`bg-neutral-100` /
`bg-neutral-950`), with no glass. A sheet or a dialog over the control panel is solid
(`bg-neutral-100` / `bg-neutral-900`): over a dimmed page, glass would look murky.

The keyboard (`./KeyboardView.tsx`, `keyboardPanel`) is the one surface with a denser tint: it is
large and full of keys, so each key must show over a white window or a black one. Light: grey
(`neutral-300`) at 70 %, as a phone's light keyboard; dark: grey at 82 %. Its top corners are
round (16 px, `rounded-t-2xl`), as a panel's. Docked, the keyboard is under the screen, not over
it, so it is a solid panel in the same colors (`bg-neutral-300` / `bg-neutral-950`), square, with
no blur.

## Flat content

Each color has its light value and its `dark:` value.

| Part | Light | Dark |
|---|---|---|
| Main text | `text-neutral-900` | `text-white` |
| Secondary text | `text-black/60` | `text-white/60` |
| Least text | `text-black/40` | `text-white/40` |
| A note (a warning) | `text-amber-700` | `text-amber-300` |
| Green text ("here") | `text-emerald-700` | `text-emerald-300` |
| A hollow (the monitors' arrangement) | `rounded-xl border-black/10 bg-white/40` | `border-white/10 bg-black/25` |
| A field, a select | `rounded-lg border-black/10 bg-white/60`, focus `border-black/30` | `border-white/10 bg-black/30`, focus `border-white/30` |
| A box (a monitor) | `rounded-lg border-black/15 bg-white` | `border-white/20 bg-neutral-800` |
| A marked box (the primary monitor) | `rounded-lg border-emerald-500 bg-emerald-50` | `border-emerald-400/80 bg-emerald-950` |
| A button | `rounded-lg border-black/15 bg-white/50`, mouse over `bg-white/80` | `border-white/15 bg-white/5`, mouse over `bg-white/10` |
| The button that ends the task (Done) | `rounded-lg bg-emerald-600 text-white`, mouse over `bg-emerald-500` | the same |
| A small icon button | `rounded-md text-black/55`, mouse over `bg-black/5` | `text-white/60`, mouse over `bg-white/10` |
| A remove button | `text-rose-600` | `text-rose-400` |
| A line between rows | `divide-black/10` | `divide-white/10` |
| A row under the mouse, under a finger | `hover:bg-black/5`, `active:bg-black/10` | `bg-white/5`, `bg-white/10` |
| A card (a clone, a ticket, an account) | `rounded-lg border-black/10 bg-white/70` | `border-white/10 bg-white/[0.06]` |
| A column of the board | `rounded-xl border-black/10 bg-black/[0.03]` | `border-white/10 bg-white/[0.03]` |
| The clone on the screen | `bg-emerald-500/10`, a 3 px emerald bar at the left | `bg-emerald-400/10` |
| The open tab, the one choice on | `bg-neutral-900 text-white` (the other look) | `bg-white text-neutral-900` |
| An error line | `bg-rose-500/15 text-rose-700` | `text-rose-300` |
| A key | `rounded-md border-black/10 bg-white/85 text-neutral-900` | `border-transparent bg-white/15 text-white` |
| A pressed key | `bg-black/15` | `bg-white/35` |
| A key that is on (Shift, Ctrl, Fn, Caps Lock) | `bg-neutral-800 text-white` (the other look) | `bg-neutral-100 text-neutral-900` |
| The dictation key, listening | `bg-rose-600 text-white` | the same |
| The keyboard's height bar | `bg-black/25`, held `bg-black/70` | `bg-white/30`, held `bg-white` |
| The trackpad's corners (the keys as a trackpad) | `border-black/35` | `border-white/40` |

## Color

- Neutral grey only: no blue in the greys (`/ng` is black and grey).
- One accent, green (Tailwind emerald): the primary monitor, Done, "here". Never on a glass
  surface itself: no green rim, no green glow.
- A count is a small solid dot: green for "to do", red (`rose-500`) for "went wrong". It has its
  own top highlight and a shadow, as a drop of colored glass.
- A danger action (remove) is red text, never a red surface.

## Shape

- A floating button is round, 44 px.
- A panel's corners: 16 px (`rounded-2xl`). A piece in it: 8 px (`rounded-lg`). A small control
  in a box: 6 px (`rounded-md`).
- The monitor's number: a 144 px square (`size-36`), always the same size, in the top-left
  corner.
- Space between floating pieces, and from the display's edges: 8 px.

## Motion

- A control's light and press: 150 ms, ease out.
- A piece that comes in or goes out (the floating buttons, the control panel): 300 ms, ease out, along
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
