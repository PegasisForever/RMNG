// One dialog frame, for every dialog on the page: the backdrop, the panel, the Escape slot
// and the exit delay, in one module.
//
// Seven dialogs used to re-declare all four by hand — the same 80-character backdrop string,
// the same `-in`/`-out` class pair, a `closing` flag on every View interface, and a
// `useModalExit()` in every container to raise that flag. The frame was therefore not
// anybody's: changing it meant changing it seven times, and a new dialog started by copying
// an old one. It lives here now, and a dialog states only what is true of itself — how wide
// it is, whether Escape may close it — and puts its own markup inside.
//
// The markup goes in as a function of `close`, the shell's own door out. `close()` plays the
// exit frames and then runs `onExited`; `close(then)` runs `then` instead, which is what a
// button that both acts and closes (Apply, Save) needs to keep the frames. `open` is the same
// door from outside, for the dialogs that close because an operation settled rather than
// because anybody clicked.
import { createContext, useCallback, useContext, useEffect, useRef, type ReactNode } from "react";

import { createPortal } from "react-dom";

import { useModalEscape } from "~/lib/useModalEscape";
import { useModalExit } from "~/lib/useModalExit";

/** The panel shapes the seven dialogs actually come in.
 *
 *  - `sm` a short form (the account and sign-in dialogs)
 *  - `md` a wider short form (port forwards)
 *  - `lg` a form tall enough to need a scrolling middle, which it pins itself
 *  - `panel` the two-pane fixed-height overlay (the clone dialog) */
export type ModalSize = "sm" | "md" | "lg" | "panel";

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

/** The panel's surface (`~/desk/GLASS.md`). Over the clone's screen (also over the wide control
 *  panel), glass: its backdrop only darkens the screen, as the panel blurs what is under it. In
 *  the control panel's own window there is no screen under it, so it is solid, in the page's
 *  look. Its corners are 26 px and its padding 16 px, so a button in a corner (10 px, the piece
 *  radius) is concentric with it (GLASS.md, Shape). Its content is flat either way
 *  (`.on-glass`: the slate parts from `~/components` read on it). */
const CHROME: Record<ModalSurface, string> = {
  glass: "on-glass glass glass-thick rounded-[26px]",
  solid:
    "on-glass rounded-[26px] border border-black/10 bg-neutral-100 text-neutral-900 shadow-[0_24px_56px_-12px_rgb(0_0_0/0.35)] dark:border-white/10 dark:bg-neutral-900 dark:text-white",
};

const PANEL: Record<ModalSize, string> = {
  sm: "max-h-[min(90vh,100%)] w-full max-w-md overflow-y-auto p-4",
  md: "w-full max-w-lg p-4",
  lg: "flex max-h-[min(90vh,100%)] w-full max-w-lg flex-col p-4",
  panel: "flex h-[42rem] max-h-[min(90vh,100%)] w-full max-w-4xl flex-col overflow-hidden",
};

/** What a window dialog's panel is made of: `glass` over the clone's screen, `solid` where there
 *  is none under the page (the control panel's own window). */
export type ModalSurface = "glass" | "solid";

const SurfaceContext = createContext<ModalSurface>("glass");

/** Every window dialog inside this has this surface. The control panel's own window gives
 *  `solid`; the page over the clone's screen needs none. */
export const ModalSurfaceProvider = SurfaceContext.Provider;

/** The surface of the window dialogs here; an open list (`AnchoredList`) takes it too. */
export const useModalSurface = () => useContext(SurfaceContext);

/** Where a dialog goes. `window`: over the whole page, on a dimmed backdrop. `inline`: it
 *  fills the nearest positioned box instead, with no backdrop and no frame. The narrow control
 *  panel shows in the space of the keyboard, and a dialog opened from one of its tabs takes that
 *  same space, so the clone's screen above stays in view. */
export type ModalPlacement = "window" | "inline";

const PlacementContext = createContext<ModalPlacement>("window");

/** Every dialog inside this opens `inline` (see `ModalPlacement`). */
export const ModalPlacementProvider = PlacementContext.Provider;

const INLINE_PANEL: Record<ModalSize, string> = {
  sm: "overflow-y-auto p-3",
  md: "overflow-y-auto p-3",
  lg: "p-3",
  panel: "overflow-hidden",
};

/** The inline frame: the box's whole space, on a solid ground of the page's look (it hides the
 *  tab under it; on the control panel's glass, glass would show the tab through). */
function inlineClasses(size: ModalSize, closing: boolean) {
  return {
    backdrop: `absolute inset-0 z-50 flex flex-col rounded-[10px] bg-neutral-100 dark:bg-neutral-900 ${
      closing ? MODAL_ANIMATION.backdrop.exit : MODAL_ANIMATION.backdrop.enter
    }`,
    panel: `flex min-h-0 w-full flex-1 flex-col ${INLINE_PANEL[size]}`,
  };
}

/** The window dialog's space round it: 1rem, plus the unsafe edges (`--safe-*`, set by
 *  `routes/app.tsx`), so a dialog keeps clear of the clock bar, the home bar and a cut-out. */
const AROUND =
  "pt-[calc(1rem+var(--safe-top,0px))] pr-[calc(1rem+var(--safe-right,0px))] " +
  "pb-[calc(1rem+var(--safe-bottom,0px))] pl-[calc(1rem+var(--safe-left,0px))]";

/** Split out of the markup so the entry/exit swap is testable without a renderer. */
export function backdropClass(closing: boolean): string {
  return (
    `fixed inset-0 z-50 flex items-center justify-center bg-black/20 dark:bg-black/35 ${AROUND} ` +
    (closing ? MODAL_ANIMATION.backdrop.exit : MODAL_ANIMATION.backdrop.enter)
  );
}

/** Split out of the markup so the entry/exit swap is testable without a renderer. */
export function panelClass(size: ModalSize, closing: boolean, surface: ModalSurface = "glass"): string {
  return (
    `${CHROME[surface]} ${PANEL[size]} ` +
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
  /** The dialog's own markup, given the shell's door out. */
  children: (close: ModalClose) => ReactNode;
}

export function ModalShell({
  open = true,
  onExited,
  dismissible = true,
  size = "sm",
  children,
}: ModalShellProps) {
  const { closing, beginExit } = useModalExit();
  const placement = useContext(PlacementContext);
  const surface = useContext(SurfaceContext);

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

  if (placement === "inline") {
    const inline = inlineClasses(size, closing);
    return (
      <div role="dialog" className={inline.backdrop}>
        <div className={inline.panel}>{children(close)}</div>
      </div>
    );
  }

  const dialog = (
    <div data-modal className={backdropClass(closing)}>
      {/* Backdrop is inert — clicking it must not close the dialog, because a stray click
          outside must never discard a half-filled form. Cancel and Escape are the ways out. */}
      <div className={panelClass(size, closing, surface)}>{children(close)}</div>
    </div>
  );
  // Over the whole page, on the body: opened from inside a box that is a layer of its own (the
  // control panel, which also moves), it would be drawn in that layer, under whatever is over
  // the box (the side panels), and its `fixed` would be the box's
  return typeof document === "undefined" ? dialog : createPortal(dialog, document.body);
}
