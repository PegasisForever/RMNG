// The three buttons of the /ng screen (`./FullDesk.tsx`), on top in the screen's top-right
// corner: the pages (usage, tickets, clones, notes, settings), the keyboard, and the fit, which
// gives the clone's monitor this window's size. A tap on the keyboard shows or hides the floating
// keyboard; a long press docks it (always shown) or undocks it. A long press on the fit opens the
// Edit layout dialog. A button whose thing is open is lit; the docked keyboard's has a bar under
// its icon; the fit is dim when the monitor fits already. A right click is a long press, for a
// mouse. In mouse and keyboard mode they have no keyboard button, and they slide up out of the
// window when they are not in use (`away`; `./FullDesk.tsx` says when).
// Where the browser has fullscreen, a fourth button turns it on and off (the page's own
// fullscreen: only that one hands the system's chords, such as Super+Space, to the page). On a
// desktop whose layout has monitors with no window open, a button at the left opens their
// windows, with how many there are on it.
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
  fits,
  badge = 0,
  onPages,
  onKeyboard,
  onKeyboardDock,
  onFit,
  onFitSettings,
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
  /** The clone's monitor has this screen's size already. */
  fits: boolean;
  /** A count on the pages button (clones that stopped working unseen). */
  badge?: number;
  onPages: () => void;
  onKeyboard: () => void;
  /** A long press on the keyboard: dock it, or undock it. */
  onKeyboardDock: () => void;
  onFit: () => void;
  /** A long press on Fit: the Edit layout dialog. */
  onFitSettings: () => void;
  /** Fullscreen: on or not, and the switch; absent where the browser has none. */
  fullscreen?: { on: boolean; toggle: () => void };
  /** The layout's monitors with no window open (their numbers), and the opening of their
   *  windows; absent when there are none, or the browser opens no windows. */
  monitors?: { missing: number[]; onOpen: () => void };
  /** Slid up out of the window, and out of reach. */
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
        // Up by their height, the room above them, and their shadow
        transform: away ? `translateY(calc(-100% - env(safe-area-inset-top) - ${FLOAT_GAP * 3}px))` : undefined,
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
      <Round label="Pages" on={pagesOpen} onClick={onPages}>
        <LayoutGrid aria-hidden className="size-5" />
        {badge ? <Count n={badge} className="bg-rose-500" /> : null}
      </Round>
      {keyboard ? (
        <Round
          label={keyboardDocked ? "Keyboard, docked" : "Keyboard"}
          on={keyboardOpen}
          onClick={onKeyboard}
          onLongPress={onKeyboardDock}
        >
          <Keyboard aria-hidden className="size-5" />
          {keyboardDocked ? <span aria-hidden className="absolute bottom-2 h-0.5 w-5 rounded-full bg-current" /> : null}
        </Round>
      ) : null}
      {fullscreen ? (
        <Round label={fullscreen.on ? "Leave fullscreen" : "Fullscreen"} on={fullscreen.on} onClick={fullscreen.toggle}>
          {fullscreen.on ? <Minimize aria-hidden className="size-5" /> : <Maximize aria-hidden className="size-5" />}
        </Round>
      ) : null}
      <Round
        label={fits ? "The clone fits this window. Hold for the layout" : "Fit the clone to this window. Hold for the layout"}
        dim={fits}
        onClick={() => !fits && onFit()}
        onLongPress={onFitSettings}
      >
        <Expand aria-hidden className="size-5" />
      </Round>
    </div>
  );
}

/** A count in a button's top-right corner, on `className`'s color. */
function Count({ n, className }: { n: number; className: string }) {
  return (
    <span
      className={`absolute -right-0.5 -top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full px-1 text-[10px] font-semibold leading-none text-white ${className}`}
    >
      {n}
    </span>
  );
}

/** A press this long is a long press. */
const LONG_PRESS_MS = 500;

function Round({
  label,
  on = false,
  dim = false,
  onClick,
  onLongPress,
  children,
}: {
  label: string;
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
      className={`relative flex items-center justify-center rounded-full border shadow-lg backdrop-blur-md ${
        on
          ? "border-neutral-200 bg-neutral-100 text-black"
          : "border-white/15 bg-black/55 text-white active:bg-neutral-700"
      } ${dim ? "opacity-40" : ""}`}
      style={{ width: FLOAT_BUTTON, height: FLOAT_BUTTON, WebkitTouchCallout: "none" }}
    >
      {children}
    </button>
  );
}
