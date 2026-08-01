// The ⋮ menu every card carries: a trigger, and a panel portalled to the body.
//
// The portal is the whole point. A card frame clips its overflow and a board column scrolls
// its own, so a menu positioned inside either one is cut off at the card's edge. Rendering
// into the body at fixed coordinates measured off the trigger is what lets it draw over the
// board instead.
//
// Every trigger and item stops pointer propagation, so opening one neither selects the card
// underneath nor starts dragging it.
import { EllipsisVertical, type LucideIcon } from "lucide-react";
import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

import {
  GLASS_DIVIDER,
  GLASS_FILL_DENSE,
  GLASS_HOVER,
  GLASS_OUTLINE,
  GLASS_SHADOW_LIFTED,
} from "~/lib/glass";

/** Lets an item close the menu it is in, including one that closes on its own delay. */
const CloseContext = createContext<() => void>(() => {});

export function useMenuClose(): () => void {
  return useContext(CloseContext);
}

export function MenuDivider() {
  return <div className={`my-1 h-px ${GLASS_DIVIDER}`} />;
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
      className={`flex w-full cursor-pointer items-center gap-1.5 px-3 py-1.5 text-left text-xs ${
        danger
          ? "text-red-600 hover:bg-red-500/10 dark:text-red-400 dark:hover:bg-red-500/20"
          : `text-slate-600 dark:text-slate-300 ${GLASS_HOVER}`
      }`}
    >
      <Icon aria-hidden className="size-4 shrink-0" />
      {label}
    </button>
  );
}

export function OverflowMenu({
  label,
  disabled = false,
  children,
}: {
  /** What the trigger announces, e.g. `actions for WE-301`. */
  label: string;
  disabled?: boolean;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  // Where the panel goes, in viewport coordinates. Null until the trigger has been measured,
  // which is also what keeps the first frame from flashing it at the origin.
  const [at, setAt] = useState<{ top: number; right: number } | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      const target = e.target as Node;
      // The panel is portalled out of `ref`, so it has to be asked separately — otherwise
      // this closes on mousedown and the item's own click never lands.
      if (ref.current?.contains(target) || menuRef.current?.contains(target)) return;
      setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    // A fixed panel cannot follow its trigger, so scrolling the column out from under it
    // closes it rather than leaving it stranded mid-board.
    const onScroll = () => setOpen(false);
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
  }, [open]);

  return (
    <div ref={ref} className="relative shrink-0" onClick={(e) => e.stopPropagation()}>
      <button
        type="button"
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        onPointerDown={(e) => e.stopPropagation()}
        onClick={(e) => {
          e.stopPropagation();
          const box = e.currentTarget.getBoundingClientRect();
          setAt({ top: box.bottom + 4, right: window.innerWidth - box.right });
          setOpen((o) => !o);
        }}
        className={`cursor-pointer rounded p-1 text-slate-400 hover:bg-slate-200 hover:text-slate-600 disabled:opacity-0 dark:text-slate-500 dark:hover:bg-slate-700 dark:hover:text-slate-300 ${
          open ? "bg-slate-200 text-slate-600 dark:bg-slate-700 dark:text-slate-300" : ""
        }`}
      >
        <EllipsisVertical className="size-4" />
      </button>
      {open && at
        ? createPortal(
            <CloseContext.Provider value={() => setOpen(false)}>
              <div
                ref={menuRef}
                role="menu"
                style={{ top: at.top, right: at.right }}
                className={`fixed z-50 w-56 overflow-hidden rounded-md py-1 ${GLASS_OUTLINE} ${GLASS_FILL_DENSE} ${GLASS_SHADOW_LIFTED}`}
                onClick={(e) => e.stopPropagation()}
              >
                {children}
              </div>
            </CloseContext.Provider>,
            document.body,
          )
        : null}
    </div>
  );
}
