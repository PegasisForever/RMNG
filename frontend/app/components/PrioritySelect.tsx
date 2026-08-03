// Linear's priority scale, picked by its own glyphs.
//
// A native `<select>` cannot draw anything but text in an option, and the priority a ticket
// carries is read as a glyph everywhere else on this board: on the card, in the ticket panel,
// and in the line under this very dialog. Typing the words in one place and drawing the bars
// in every other makes them look like two different properties, so this is a listbox rather
// than a select.
//
// Pure. It holds one piece of state, whether its menu is open, which is exactly the ephemeral
// kind a leaf keeps: nothing above it needs to know, and nothing is lost when it closes.
import { ChevronDown } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { PRIORITY_LABEL, PriorityIcon } from "~/components/TicketColumn";
import { useModalEscape } from "~/lib/useModalEscape";

/** The scale in the order Linear reads it: unranked first, then urgent down to low. */
export const PRIORITY_LEVELS = [0, 1, 2, 3, 4];

export interface PrioritySelectProps {
  /** Linear's own value: 0 unranked, 1 urgent, 2 high, 3 medium, 4 low. */
  value: number;
  onChange: (level: number) => void;
  disabled?: boolean;
  /** The field styling of whatever form this sits in, so the trigger matches its neighbours. */
  className?: string;
}

export function PrioritySelect({ value, onChange, disabled, className }: PrioritySelectProps) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement | null>(null);

  // A click anywhere else dismisses it, which is what every menu on a page does. Pointerdown
  // rather than click: a press that starts outside has already decided to leave, and waiting
  // for the release leaves the menu up over whatever is being clicked.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", onDown);
    return () => document.removeEventListener("pointerdown", onDown);
  }, [open]);

  return (
    <div ref={root} className="relative">
      <button
        type="button"
        disabled={disabled}
        onClick={() => setOpen((was) => !was)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" || e.key === "ArrowUp") {
            e.preventDefault();
            setOpen(true);
          }
        }}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={`Priority: ${PRIORITY_LABEL[value]}`}
        // `h-9` because this stands in a row of native `<select>`s, and a select lays its text
        // out at the browser's `line-height: normal` (36px here) while a button inherits the
        // form's own 20px and comes out two pixels taller. Pinned here rather than asked of
        // each caller: lining up with a select is this control's whole job.
        className={`flex h-9 items-center gap-2 text-left disabled:opacity-50 ${className ?? ""}`}
      >
        <PriorityIcon level={value} />
        <span className="min-w-0 flex-1 truncate">{PRIORITY_LABEL[value]}</span>
        <ChevronDown aria-hidden className="size-3.5 shrink-0 text-slate-400" />
      </button>

      {open ? (
        <PriorityMenu
          value={value}
          onPick={(level) => {
            onChange(level);
            setOpen(false);
          }}
          onDismiss={() => setOpen(false)}
        />
      ) : null}
    </div>
  );
}

/** The open list.
 *
 *  It is its own component so that it mounts and unmounts with the menu, which is what puts it
 *  on the modal stack for exactly as long as it is up: Escape then closes the menu, and the
 *  dialog underneath keeps its own Escape for when the menu is shut. */
function PriorityMenu({
  value,
  onPick,
  onDismiss,
}: {
  value: number;
  onPick: (level: number) => void;
  onDismiss: () => void;
}) {
  const list = useRef<HTMLUListElement | null>(null);
  // Which row the arrows are on. It starts on the current value, so opening and pressing Enter
  // changes nothing, and moves independently of the selection until one is committed.
  const [active, setActive] = useState(() => Math.max(0, PRIORITY_LEVELS.indexOf(value)));

  useModalEscape(onDismiss);
  useEffect(() => list.current?.focus(), []);

  return (
    <ul
      ref={list}
      role="listbox"
      tabIndex={-1}
      aria-label="Priority"
      aria-activedescendant={`priority-option-${PRIORITY_LEVELS[active]}`}
      onKeyDown={(e) => {
        if (e.key === "ArrowDown" || e.key === "ArrowUp") {
          e.preventDefault();
          const step = e.key === "ArrowDown" ? 1 : -1;
          setActive((i) => (i + step + PRIORITY_LEVELS.length) % PRIORITY_LEVELS.length);
        }
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onPick(PRIORITY_LEVELS[active]);
        }
        // Tab leaves the field, so it commits nothing and shuts the menu rather than moving
        // focus into a list the operator has already stepped past.
        if (e.key === "Tab") onDismiss();
      }}
      className="absolute z-10 mt-1 w-full overflow-hidden rounded-md border border-slate-200 bg-white py-1 shadow-lg outline-none dark:border-slate-600 dark:bg-slate-800"
    >
      {PRIORITY_LEVELS.map((level, i) => (
        <li
          key={level}
          id={`priority-option-${level}`}
          role="option"
          aria-selected={level === value}
          onPointerDown={(e) => {
            // The document listener above closes on any pointerdown outside; this one is
            // inside, and the default would move focus off the list before the click lands.
            e.preventDefault();
            onPick(level);
          }}
          onPointerEnter={() => setActive(i)}
          className={`flex cursor-pointer items-center gap-2 px-3 py-1.5 text-sm ${
            i === active ? "bg-slate-100 dark:bg-slate-700" : ""
          } ${
            level === value
              ? "font-medium text-slate-900 dark:text-slate-100"
              : "text-slate-600 dark:text-slate-300"
          }`}
        >
          <PriorityIcon level={level} />
          <span className="min-w-0 flex-1 truncate">{PRIORITY_LABEL[level]}</span>
        </li>
      ))}
    </ul>
  );
}
