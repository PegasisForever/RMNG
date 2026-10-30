// The three buttons of the /ng screen (`./FullDesk.tsx`), always on top in the screen's top-right
// corner: the pages (usage, tickets, clones, notes, settings), the keyboard, and the fit, which
// gives the clone's monitor this screen's size. A tap on the keyboard shows or hides the floating
// keyboard; a long press docks it (always shown) or undocks it. A long press on the fit opens the
// pixel ratio setting. A button whose thing is open is lit; the docked keyboard's has a bar under
// its icon; the fit is dim when the monitor fits already.
import { Expand, Keyboard, LayoutGrid } from "lucide-react";
import { type ReactNode, useRef } from "react";

/** One button's size, and the space between them, CSS px. */
export const FLOAT_BUTTON = 44;
export const FLOAT_GAP = 8;

export function FloatingButtons({
  pagesOpen,
  keyboardOpen,
  keyboardDocked,
  fits,
  badge = 0,
  onPages,
  onKeyboard,
  onKeyboardDock,
  onFit,
  onFitSettings,
}: {
  pagesOpen: boolean;
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
  /** A long press on Fit: the setting of its pixel ratio. */
  onFitSettings: () => void;
}) {
  return (
    <div
      className="absolute z-50 flex"
      style={{
        gap: FLOAT_GAP,
        top: `calc(env(safe-area-inset-top) + ${FLOAT_GAP}px)`,
        right: `calc(env(safe-area-inset-right) + ${FLOAT_GAP}px)`,
      }}
    >
      <Round label="Pages" on={pagesOpen} onClick={onPages}>
        <LayoutGrid aria-hidden className="size-5" />
        {badge ? (
          <span className="absolute -right-0.5 -top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-rose-500 px-1 text-[10px] font-semibold leading-none text-white">
            {badge}
          </span>
        ) : null}
      </Round>
      <Round
        label={keyboardDocked ? "Keyboard, docked" : "Keyboard"}
        on={keyboardOpen}
        onClick={onKeyboard}
        onLongPress={onKeyboardDock}
      >
        <Keyboard aria-hidden className="size-5" />
        {keyboardDocked ? <span aria-hidden className="absolute bottom-2 h-0.5 w-5 rounded-full bg-current" /> : null}
      </Round>
      <Round
        label={fits ? "The clone fits this screen" : "Fit the clone to this screen"}
        dim={fits}
        onClick={() => !fits && onFit()}
        onLongPress={onFitSettings}
      >
        <Expand aria-hidden className="size-5" />
      </Round>
    </div>
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
      onPointerDown={() => {
        long.current = false;
        if (!onLongPress) return;
        stop();
        timer.current = setTimeout(() => {
          long.current = true;
          onLongPress();
        }, LONG_PRESS_MS);
      }}
      onPointerUp={stop}
      onPointerCancel={stop}
      onPointerLeave={stop}
      onContextMenu={(e) => e.preventDefault()}
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
