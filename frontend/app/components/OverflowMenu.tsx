// The ⋮ menu every card carries: a trigger, and a panel portalled to the body.
//
// The portal is the whole point. A card frame clips its overflow and a column scrolls its
// own, so a menu positioned inside either one is cut off at the card's edge. Rendering into
// the body at fixed coordinates measured off the trigger is what lets it draw over them
// instead.
//
// Every trigger and item stops pointer propagation, so opening one neither selects the card
// underneath nor starts dragging it.
import { Check, EllipsisVertical, type LucideIcon } from "lucide-react";
import {
  createContext,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

import { useModalSurface } from "~/components/ModalShell";
// Only in the components below, never at the top of this module: `ui` imports this module too
import { DANGER_TEXT, FLOAT_SOLID, ICON_BUTTON, LINE, PRESS, R_PIECE, TEXT, TEXT_2, TEXT_3 } from "~/desk/control/ui";

/** Lets an item close the menu it is in, including one that closes on its own delay. */
const CloseContext = createContext<() => void>(() => {});

export function useMenuClose(): () => void {
  return useContext(CloseContext);
}

/** The padding and text size every row shares, so a note lines up with the items above it. */
export const MENU_ROW = "flex w-full items-center gap-1.5 px-2.5 py-1.5 text-left text-xs";

export function MenuDivider() {
  return <div className={`mx-1 my-1.5 border-t ${LINE}`} />;
}

/** One row. The icon is required rather than optional: a menu where some items carry one and
 *  some do not reads as a list with holes in it, and the labels stop lining up. */
export function MenuItem({
  icon: Icon,
  label,
  onClick,
  danger = false,
}: {
  icon: LucideIcon;
  label: string;
  onClick: () => void;
  danger?: boolean;
}) {
  const close = useMenuClose();
  return (
    <button
      type="button"
      role="menuitem"
      onPointerDown={(e) => e.stopPropagation()}
      onClick={(e) => {
        e.stopPropagation();
        close();
        onClick();
      }}
      className={`${MENU_ROW} ${R_PIECE} cursor-pointer ${danger ? DANGER_TEXT : TEXT} ${PRESS}`}
    >
      <Icon aria-hidden className="size-4 shrink-0" />
      {label}
    </button>
  );
}

/** One row of a menu that picks a value rather than running an action.
 *
 *  The mark is a slot instead of a Lucide icon, because the two menus that pick things pick
 *  them by a mark this component could not draw: a workflow state's ring, and a label's own
 *  colour out of Linear. The tick on the right is the current value, which is what makes the
 *  list readable as a set of choices rather than a set of commands. */
export function MenuChoice({
  icon,
  label,
  selected = false,
  onClick,
}: {
  icon: ReactNode;
  label: string;
  selected?: boolean;
  onClick: () => void;
}) {
  const close = useMenuClose();
  return (
    <button
      type="button"
      role="menuitemradio"
      aria-checked={selected}
      onPointerDown={(e) => e.stopPropagation()}
      onClick={(e) => {
        e.stopPropagation();
        close();
        onClick();
      }}
      className={`${MENU_ROW} ${R_PIECE} cursor-pointer ${TEXT} ${PRESS}`}
    >
      <span className="flex size-4 shrink-0 items-center justify-center">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {selected ? (
        <Check aria-hidden className={`size-3.5 shrink-0 ${TEXT_2}`} />
      ) : null}
    </button>
  );
}

/** A line of text where rows would be: the menu is loading, or has nothing to offer. Says so
 *  rather than opening empty, which reads as a menu that is broken. */
export function MenuNote({ children }: { children: ReactNode }) {
  return <p className={`${MENU_ROW} ${TEXT_3}`}>{children}</p>;
}

/** How far the panel sits off its trigger, and the least it keeps from a viewport edge. */
const GAP = 4;
const EDGE = 8;

/** A rectangle in viewport coordinates — a trigger's box, or the viewport itself. */
export interface Box {
  top: number;
  bottom: number;
  left: number;
  right: number;
}

/** Where a panel of this size goes: hanging off `trigger`, wholly inside `viewport`.
 *
 *  A fixed panel is clipped by nothing, so nothing stops it drawing off the bottom of the
 *  window — which is what a ⋮ on a card near the foot of a column used to do, leaving half
 *  its items unreachable. This flips the panel above its trigger when the room below will not
 *  hold it, and caps its height to whichever side it lands on so a menu taller than the whole
 *  window scrolls instead of overflowing. The horizontal clamp does the same for a trigger
 *  close enough to an edge that the panel would hang off it.
 *
 *  Pure, so a test can pin the flip and the clamps without mounting anything. Returns
 *  `maxHeight` rather than a height: the panel keeps its natural size when it fits.
 */
export function placeMenu(
  trigger: Box,
  panel: { width: number; height: number },
  viewport: { width: number; height: number },
  align: "left" | "right",
): { top: number; left: number; maxHeight: number } {
  const below = viewport.height - EDGE - (trigger.bottom + GAP);
  const above = trigger.top - GAP - EDGE;

  let top: number;
  let maxHeight: number;
  // Below unless it does not fit and above is roomier: a menu that fits stays where the
  // operator expects it, and one that fits neither way opens on the taller side.
  if (panel.height <= below || below >= above) {
    top = trigger.bottom + GAP;
    maxHeight = Math.max(below, 0);
  } else {
    maxHeight = Math.max(above, 0);
    top = trigger.top - GAP - Math.min(panel.height, maxHeight);
  }

  const wanted = align === "left" ? trigger.left : trigger.right - panel.width;
  const rightmost = Math.max(EDGE, viewport.width - EDGE - panel.width);
  return { top, left: Math.min(Math.max(wanted, EDGE), rightmost), maxHeight };
}

/** The trigger's own styling, which only a custom trigger sets. The ⋮ button brings its own. */
const PLAIN_TRIGGER = "flex cursor-pointer items-center rounded-md disabled:opacity-50";

/** A menu's panel, portalled to the body at fixed coordinates off `at` (its trigger's box):
 *  placed to stay inside the window (`placeMenu`), closed by a click outside it, Escape, a
 *  scroll that would strand it, or an item (`useMenuClose`). Its surface is thick glass
 *  (`~/desk/GLASS.md`), on the body so it is never glass inside a glass column; solid in the
 *  control panel's own window, where no clone's screen is under it. Its rows are flat, 10 px
 *  corners set 6 px into its 16 px ones. */
export function MenuPanel({
  at,
  align = "right",
  onClose,
  ignore,
  children,
}: {
  at: Box;
  align?: "left" | "right";
  onClose: () => void;
  /** Its trigger: a press on it is the trigger's to handle, not a click outside. */
  ignore?: { current: HTMLElement | null };
  children: ReactNode;
}) {
  // Where the panel actually goes, which takes measuring the panel itself — see the layout
  // effect below. It renders hidden for the one pass that measurement needs.
  const [place, setPlace] = useState<{ top: number; left: number; maxHeight: number } | null>(null);
  const surface = useModalSurface() === "solid" ? FLOAT_SOLID : "glass glass-thick";
  const menuRef = useRef<HTMLDivElement>(null);
  // What scrolls inside the glass, so the glass's rim stays put while the rows move
  const listRef = useRef<HTMLDivElement>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  // Placing the panel needs its size, and its size is only known once it is in the document.
  // A layout effect runs before the browser paints, so the unplaced pass is never seen.
  //
  // `scrollHeight` rather than the rendered height: once a previous pass has capped the
  // panel, its rendered height is that cap, and measuring it again would leave a menu stuck
  // short after the window grew. The observer re-places a menu whose rows arrive late — the
  // ones that open on a note and fill in from Linear.
  useLayoutEffect(() => {
    const el = menuRef.current;
    const list = listRef.current;
    if (!el || !list) return;
    const measure = () => {
      setPlace(
        placeMenu(
          at,
          { width: el.offsetWidth, height: list.scrollHeight },
          { width: window.innerWidth, height: window.innerHeight },
          align,
        ),
      );
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(list);
    return () => observer.disconnect();
  }, [at, align]);

  useEffect(() => {
    const close = () => closeRef.current();
    const onDoc = (e: MouseEvent) => {
      const target = e.target as Node;
      // The panel is portalled out of its trigger, so it has to be asked separately —
      // otherwise this closes on mousedown and the item's own click never lands.
      if (ignore?.current?.contains(target) || menuRef.current?.contains(target)) return;
      close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    // A fixed panel cannot follow its trigger, so scrolling the column out from under it
    // closes it rather than leaving it stranded mid-column.
    //
    // Its own scrolling is exempt. This listens in the capture phase, which is what catches a
    // scrolling ancestor at all, and that also catches a menu long enough to scroll inside —
    // so a list of labels would close itself the moment you reached for the one at the bottom.
    // A resize passes `window` as its target, which is not a Node and so never matches.
    const onScroll = (e: Event) => {
      if (e.target instanceof Node && menuRef.current?.contains(e.target)) return;
      close();
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onScroll);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onScroll);
    };
  }, [ignore]);

  return createPortal(
    <CloseContext.Provider value={onClose}>
      <div
        ref={menuRef}
        role="menu"
        style={{
          top: place ? place.top : at.bottom + GAP,
          left: place ? place.left : at.left,
          maxHeight: place?.maxHeight,
          visibility: place ? undefined : "hidden",
        }}
        className={`fixed z-50 flex w-56 flex-col rounded-2xl ${surface}`}
        onClick={(e) => e.stopPropagation()}
      >
        <div ref={listRef} className="min-h-0 overflow-y-auto overscroll-y-contain p-1.5">
          {children}
        </div>
      </div>
    </CloseContext.Provider>,
    document.body,
  );
}

export function OverflowMenu({
  label,
  disabled = false,
  trigger,
  align = "right",
  onTouch,
  children,
}: {
  /** What the trigger announces, e.g. `actions for WE-301`. */
  label: string;
  disabled?: boolean;
  /** What the trigger draws, in place of the ⋮ glyph. Takes the open flag, so a trigger can
   *  show that its own menu is up. The button and everything wired to it stay the same. */
  trigger?: (open: boolean) => ReactNode;
  /** Which edge the panel lines up with. A card's ⋮ sits at its right corner and the panel
   *  hangs left off it; a menu off a mark at the left of a row hangs the other way. */
  align?: "left" | "right";
  /** A tap with a finger (or a pen) calls this in place of opening the panel: the control
   *  panel opens its own sheet of large tiles on a touch screen. Absent ⇒ the panel opens. */
  onTouch?: () => void;
  children: ReactNode;
}) {
  // The trigger's box in viewport coordinates, measured on the click that opens the menu; null
  // while the menu is closed.
  const [at, setAt] = useState<Box | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const open = at !== null;

  return (
    <div ref={ref} className="relative shrink-0" onClick={(e) => e.stopPropagation()}>
      <button
        type="button"
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        // Neither a pointer, a mouse nor a touch sensor starts dragging the card under it
        onPointerDown={(e) => e.stopPropagation()}
        onMouseDown={(e) => e.stopPropagation()}
        onTouchStart={(e) => e.stopPropagation()}
        onClick={(e) => {
          e.stopPropagation();
          const kind = (e.nativeEvent as PointerEvent).pointerType;
          if (onTouch && (kind === "touch" || kind === "pen")) return onTouch();
          const box = e.currentTarget.getBoundingClientRect();
          setAt(open ? null : { top: box.top, bottom: box.bottom, left: box.left, right: box.right });
        }}
        className={
          trigger
            ? PLAIN_TRIGGER
            : // Open: the fill it has under the mouse stays
              `cursor-pointer p-1 disabled:opacity-0 ${ICON_BUTTON} ${open ? "bg-black/5 dark:bg-white/10" : ""}`
        }
      >
        {trigger ? trigger(open) : <EllipsisVertical className="size-4" />}
      </button>
      {at ? (
        <MenuPanel at={at} align={align} onClose={() => setAt(null)} ignore={ref}>
          {children}
        </MenuPanel>
      ) : null}
    </div>
  );
}
