// The floating buttons of the /ng screen (`./FullDesk.tsx`), on top in the screen's top-right
// corner, right to left:
//
// - Fit (only where the browser opens no windows of its own: a phone, a tablet): gives the
//   clone's monitor this window's size; dim when it has it already. A long press opens the Edit
//   layout dialog.
// - Fullscreen (where the browser has it): the page's own fullscreen, the only one that hands
//   the system's chords, such as Super+Space, to the page. A long press opens the Edit layout
//   dialog.
// - Keyboard (touch mode only): a tap shows or hides the floating keyboard; a long press docks
//   it (always shown) or undocks it; a bar under its icon while docked.
// - Pages: the pages (usage, tickets, clones, notes, settings), with a count of the clones that
//   stopped working unseen.
// - Monitors (a desktop whose layout has monitors with no window open): opens their windows,
//   with how many there are on it.
//
// They are glass (`./GLASS.md`). A button whose thing is open is lit, but not Fullscreen: its
// icon says which way it goes. A right click is a long press, for a mouse. With a mouse each
// button says what it does, and what its long press does, in a tooltip under it. In mouse and
// keyboard mode they slide out of the window to the right when they are not in use (`away`;
// `./FullDesk.tsx` says when).
import { Expand, Keyboard, LayoutGrid, Maximize, Minimize, Monitor } from "lucide-react";
import { type ReactNode, useRef } from "react";

/** One button's size, and the space between them, CSS px. */
export const FLOAT_BUTTON = 44;
export const FLOAT_GAP = 8;

export function FloatingButtons({
  pagesOpen,
  keyboard = true,
  keyboardOpen,
  keyboardDocked,
  fit,
  badge = 0,
  onPages,
  onKeyboard,
  onKeyboardDock,
  onLayout,
  fullscreen,
  monitors,
  away = false,
}: {
  pagesOpen: boolean;
  /** The keyboard's button shows (not in mouse and keyboard mode). */
  keyboard?: boolean;
  keyboardOpen: boolean;
  /** The keyboard is docked: always shown, under the clone's screen. */
  keyboardDocked: boolean;
  /** Fit: whether the clone's monitor has this window's size already, and the fit; absent where
   *  the browser opens windows of its own (the Edit layout dialog fits each there). */
  fit?: { fits: boolean; onFit: () => void };
  /** A count on the pages button (clones that stopped working unseen). */
  badge?: number;
  onPages: () => void;
  onKeyboard: () => void;
  /** A long press on the keyboard: dock it, or undock it. */
  onKeyboardDock: () => void;
  /** A long press on Fit or on Fullscreen: the Edit layout dialog. */
  onLayout: () => void;
  /** Fullscreen: on or not, and the switch; absent where the browser has none. */
  fullscreen?: { on: boolean; toggle: () => void };
  /** The layout's monitors with no window open (their numbers), and the opening of their
   *  windows; absent when there are none, or the browser opens no windows. */
  monitors?: { missing: number[]; onOpen: () => void };
  /** Slid out of the window to the right, and out of reach. */
  away?: boolean;
}) {
  return (
    <div
      data-ng-buttons
      inert={away}
      className="absolute z-50 flex transition-transform duration-300 ease-out"
      style={{
        gap: FLOAT_GAP,
        top: `calc(env(safe-area-inset-top) + ${FLOAT_GAP}px)`,
        right: `calc(env(safe-area-inset-right) + ${FLOAT_GAP}px)`,
        // Right by their width, the room beside them, and their shadow
        transform: away ? `translateX(calc(100% + env(safe-area-inset-right) + ${FLOAT_GAP * 3}px))` : undefined,
      }}
    >
      {monitors?.missing.length ? (
        <Round
          label={`Open the window${monitors.missing.length > 1 ? "s" : ""} of monitor${
            monitors.missing.length > 1 ? "s" : ""
          } ${monitors.missing.join(", ")}`}
          onClick={monitors.onOpen}
        >
          <Monitor aria-hidden className="size-5" />
          <Count n={monitors.missing.length} className="bg-emerald-500" />
        </Round>
      ) : null}
      <Round label={pagesOpen ? "Hide the pages" : "Pages: usage, tickets, clones, notes, settings"} on={pagesOpen} onClick={onPages}>
        <LayoutGrid aria-hidden className="size-5" />
        {badge ? <Count n={badge} className="bg-rose-500" /> : null}
      </Round>
      {keyboard ? (
        <Round
          label={keyboardDocked ? "Keyboard, docked" : keyboardOpen ? "Hide the keyboard" : "Keyboard"}
          hold={keyboardDocked ? "undock it" : "dock it under the screen"}
          on={keyboardOpen}
          onClick={onKeyboard}
          onLongPress={onKeyboardDock}
        >
          <Keyboard aria-hidden className="size-5" />
          {keyboardDocked ? <span aria-hidden className="absolute bottom-2 h-0.5 w-5 rounded-full bg-current" /> : null}
        </Round>
      ) : null}
      {fullscreen ? (
        <Round
          label={fullscreen.on ? "Leave fullscreen" : "Fullscreen"}
          hold="edit the layout"
          onClick={fullscreen.toggle}
          onLongPress={onLayout}
        >
          {fullscreen.on ? <Minimize aria-hidden className="size-5" /> : <Maximize aria-hidden className="size-5" />}
        </Round>
      ) : null}
      {fit ? (
        <Round
          label={fit.fits ? "The clone fits this window" : "Fit the clone to this window"}
          hold="edit the layout"
          dim={fit.fits}
          onClick={() => !fit.fits && fit.onFit()}
          onLongPress={onLayout}
        >
          <Expand aria-hidden className="size-5" />
        </Round>
      ) : null}
    </div>
  );
}

/** A count in a button's top-right corner, on `className`'s color: a drop of colored glass
 *  (`./GLASS.md`). */
function Count({ n, className }: { n: number; className: string }) {
  return (
    <span
      className={`absolute -right-0.5 -top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full px-1 text-[10px] font-semibold leading-none text-white shadow-[inset_0_1px_0_rgb(255_255_255/0.4),0_1px_2px_rgb(0_0_0/0.45)] ${className}`}
    >
      {n}
    </span>
  );
}

/** A press this long is a long press. */
const LONG_PRESS_MS = 500;

function Round({
  label,
  hold,
  on = false,
  dim = false,
  onClick,
  onLongPress,
  children,
}: {
  label: string;
  /** What the long press does, for the tooltip. */
  hold?: string;
  on?: boolean;
  dim?: boolean;
  onClick: () => void;
  /** Absent: a long press is a click. */
  onLongPress?: () => void;
  children: ReactNode;
}) {
  // The wait for a long press, and whether it came (then the click that ends it does nothing)
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const long = useRef(false);
  const stop = () => clearTimeout(timer.current);
  return (
    // The tooltip is the button's sibling: in the button, its blur would see only the button
    <div className="group relative">
      <button
        type="button"
        aria-label={label}
        aria-pressed={on}
        onPointerDown={(e) => {
          long.current = false;
          if (!onLongPress || e.button !== 0) return;
          stop();
          timer.current = setTimeout(() => {
            long.current = true;
            onLongPress();
          }, LONG_PRESS_MS);
        }}
        onPointerUp={stop}
        onPointerCancel={stop}
        onPointerLeave={stop}
        onContextMenu={(e) => {
          e.preventDefault();
          // A mouse's right click: the long press at once
          if (onLongPress && e.button === 2) {
            stop();
            onLongPress();
          }
        }}
        onClick={() => {
          if (!long.current) onClick();
          long.current = false;
        }}
        className={`ng-glass ng-glass-control flex items-center justify-center rounded-full ${on ? "ng-glass-lit" : ""}`}
        style={{ width: FLOAT_BUTTON, height: FLOAT_BUTTON, WebkitTouchCallout: "none" }}
      >
        <span className={`contents ${dim ? "[&>*]:opacity-40" : ""}`}>{children}</span>
      </button>
      {/* Under the button, from its right edge, so it stays in the window; only with a mouse
          (Tailwind's hover needs a device that hovers) */}
      <span
        role="tooltip"
        className="ng-glass ng-glass-thick pointer-events-none absolute right-0 top-full z-10 mt-2 flex flex-col items-end whitespace-nowrap rounded-lg px-2.5 py-1.5 text-xs opacity-0 transition-opacity group-hover:opacity-100 group-hover:delay-300"
      >
        <span>{label}</span>
        {hold ? <span className="text-black/55 dark:text-white/60">Right-click or hold: {hold}</span> : null}
      </span>
    </div>
  );
}
