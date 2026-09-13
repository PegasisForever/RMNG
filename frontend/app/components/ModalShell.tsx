// One dialog frame, for every dialog on the page: the backdrop, the panel, the Escape slot
// and the exit delay, in one module.
//
// Seven dialogs used to re-declare all four by hand — the same 80-character backdrop string,
// the same `-in`/`-out` class pair, a `closing` flag on every View interface, and a
// `useModalExit()` in every container to raise that flag. The frame was therefore not
// anybody's: changing it meant changing it seven times, and a new dialog started by copying
// an old one. It lives here now, and a dialog states only what is true of itself — how wide
// it is, which z rung it sits on, whether Escape may close it — and puts its own markup
// inside.
//
// The markup goes in as a function of `close`, the shell's own door out. `close()` plays the
// exit frames and then runs `onExited`; `close(then)` runs `then` instead, which is what a
// button that both acts and closes (Apply, Save) needs to keep the frames. `open` is the same
// door from outside, for the dialogs that close because an operation settled rather than
// because anybody clicked.
import { useCallback, useEffect, useRef, type ReactNode } from "react";

import { useModalEscape } from "~/lib/useModalEscape";
import { useModalExit } from "~/lib/useModalExit";

/** The panel shapes the seven dialogs actually come in.
 *
 *  - `sm` a short form (the account and sign-in dialogs)
 *  - `md` a wider short form (port forwards)
 *  - `lg` a form tall enough to need a scrolling middle, which it pins itself
 *  - `panel` the two-pane fixed-height overlay (the clone dialog, Settings) */
export type ModalSize = "sm" | "md" | "lg" | "panel";

/** Which z rung the dialog sits on. `over` is for a dialog opened from another one — the
 *  sign-in dialog over Settings — and must stay above `base` for the Escape stack's LIFO
 *  order to match what the operator sees. */
export type ModalLayer = "base" | "over";

/** The shell's door out, as the markup inside it receives it. `close()` plays the exit frames
 *  and then runs `onExited`; `close(then)` runs `then` instead, for a button that both acts
 *  and closes. */
export type ModalClose = (then?: () => void) => void;

/**
 * Entry and exit class per element, as a pair rather than one class with a direction.
 *
 * The two names in a pair MUST differ. Flipping `animation-direction` on an already-finished
 * same-name animation does not restart it, so an exit built that way silently never ran: the
 * dialog sat frozen until the unmount timer fired. The `-in` → `-out` NAME change is the
 * restart. `app.css` keeps the two keyframe blocks mirrored by hand for the same reason.
 */
export const MODAL_ANIMATION = {
  backdrop: { enter: "rmng-backdrop-in", exit: "rmng-backdrop-out" },
  panel: { enter: "rmng-modal-in", exit: "rmng-modal-out" },
} as const;

/** How long `app.css` runs both the entry and the exit frames. `MODAL_EXIT_MS` must cover it. */
export const MODAL_ANIMATION_MS = 200;

const LAYER: Record<ModalLayer, string> = {
  base: "z-50",
  over: "z-[60]",
};

const PANEL: Record<ModalSize, string> = {
  sm: "max-h-[90vh] w-full max-w-md overflow-y-auto p-5",
  md: "w-full max-w-lg p-5",
  lg: "flex max-h-[90vh] w-full max-w-lg flex-col p-5",
  panel:
    "flex h-[42rem] max-h-[90vh] w-full max-w-4xl flex-col overflow-hidden",
};

/** The panel's own surface, identical in all seven dialogs. */
const PANEL_CHROME =
  "rounded-xl border border-slate-200 bg-white shadow-xl dark:border-slate-700 dark:bg-slate-800";

/** Split out of the markup so the entry/exit swap is testable without a renderer. */
export function backdropClass(layer: ModalLayer, closing: boolean): string {
  return (
    `fixed inset-0 ${LAYER[layer]} flex items-center justify-center bg-slate-900/30 p-4 ` +
    (closing ? MODAL_ANIMATION.backdrop.exit : MODAL_ANIMATION.backdrop.enter)
  );
}

/** Split out of the markup so the entry/exit swap is testable without a renderer. */
export function panelClass(size: ModalSize, closing: boolean): string {
  return (
    `${PANEL_CHROME} ${PANEL[size]} ` +
    (closing ? MODAL_ANIMATION.panel.exit : MODAL_ANIMATION.panel.enter)
  );
}

export interface ModalShellProps {
  /** The dialog is on screen. Flip it false and the shell plays the exit frames and then
   *  calls `onExited` — unmount there, never before. Leave it out for a dialog that only
   *  ever closes from a click inside it. */
  open?: boolean;
  /** The exit frames have run and the dialog may leave the tree. Whatever unmounts it goes
   *  here. `close(then)` runs `then` in its place for that one close. */
  onExited: () => void;
  /** Escape is live. False while an operation the dialog started is still running, so it
   *  cannot be closed out from under. The shell keeps its slot in the Escape stack either
   *  way, so the key is swallowed rather than falling through to the dialog underneath. It
   *  does not gate `close`: a dialog that closes itself when its operation settles is busy
   *  at exactly that moment. */
  dismissible?: boolean;
  size?: ModalSize;
  layer?: ModalLayer;
  /** The dialog's own markup, given the shell's door out. */
  children: (close: ModalClose) => ReactNode;
}

export function ModalShell({
  open = true,
  onExited,
  dismissible = true,
  size = "sm",
  layer = "base",
  children,
}: ModalShellProps) {
  const { closing, beginExit } = useModalExit();

  // Read the current `onExited` through a ref so `close` keeps one identity for the life of
  // the dialog. `useModalEscape` re-registers on a new callback, and re-registering means
  // leaving and re-entering the Escape stack — with two dialogs open, a fresh arrow on every
  // render would reshuffle whose Escape it is.
  const exited = useRef(onExited);
  useEffect(() => {
    exited.current = onExited;
  }, [onExited]);

  const close: ModalClose = useCallback(
    (then?: () => void) => beginExit(then ?? (() => exited.current())),
    [beginExit],
  );

  useEffect(() => {
    if (!open) close();
  }, [open, close]);

  // Escape closes regardless of focus, from a document-level listener. It has to be
  // document-level: the backdrop click does not close, and a dialog that autofocuses nothing
  // opens with the focus still on the button that launched it, so a React `onKeyDown` on the
  // panel would never fire. The stack in `useModalEscape` is what keeps two open dialogs from
  // both closing on one press.
  useModalEscape(close, dismissible);

  return (
    <div className={backdropClass(layer, closing)}>
      {/* Backdrop is inert — clicking it must not close the dialog, because a stray click
          outside must never discard a half-filled form. Cancel and Escape are the ways out. */}
      <div className={panelClass(size, closing)}>{children(close)}</div>
    </div>
  );
}
