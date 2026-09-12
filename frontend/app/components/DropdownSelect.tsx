// The app's dropdown: a custom listbox in the PrioritySelect pattern. A native
// `<select>` draws only text and answers to the OS, not the theme; every menu here
// reads and behaves the same instead — same trigger, same keyboard map, same Escape
// stacking — whatever it picks.
//
// Pure like PrioritySelect: the only state is whether the menu is open, kept in the
// leaf. Callers hand options down and get the picked value back.
import { ChevronDown } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import type { ReactNode } from "react";

import { useModalEscape } from "~/lib/useModalEscape";

/** One row in the menu: a pickable option, or a non-selectable section header. */
export type DropdownRow =
  | { value: string; label: string; disabled?: boolean }
  | { header: string };

export interface DropdownSelectProps {
  /** Every row, in menu order. Headers render as muted captions between options. */
  rows: DropdownRow[];
  /** The picked value. Should match one option; unmatched shows `placeholder`. */
  value: string;
  onChange: (value: string) => void;
  /** Shown when `value` matches no option (an empty list, a still-loading lookup). */
  placeholder?: string;
  /** Leading glyph in the trigger, e.g. the priority bars. The menu rows carry none. */
  icon?: ReactNode;
  /** Accessibility name for the trigger and the menu. Defaults to the visible label. */
  label?: string;
  disabled?: boolean;
  /** The field styling of whatever form this sits in, so the trigger matches its neighbours. */
  className?: string;
}

const isOption = (
  row: DropdownRow,
): row is { value: string; label: string; disabled?: boolean } =>
  "value" in row;

export function DropdownSelect({
  rows,
  value,
  onChange,
  placeholder = "",
  icon,
  label,
  disabled,
  className,
}: DropdownSelectProps) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement | null>(null);
  const menuId = useId();

  // A click anywhere else dismisses it, which is what every menu on a page does.
  // Pointerdown rather than click: a press that starts outside has already decided to
  // leave, and waiting for the release leaves the menu up over whatever is clicked.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", onDown);
    return () => document.removeEventListener("pointerdown", onDown);
  }, [open]);

  const current = rows.find((r) => isOption(r) && r.value === value);
  const shown = current && isOption(current) ? current.label : placeholder;

  return (
    <div ref={root} className="relative min-w-0">
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
        aria-label={label ?? (shown || undefined)}
        className={`flex w-full items-center gap-2 text-left disabled:opacity-50 ${className ?? ""}`}
      >
        {icon}
        {/* `min-h-[1lh]` pins the trigger to one text line even when it shows
            nothing: an empty inline span contributes no height, so the button
            collapsed to the chevron and jumped ~6px the moment a value landed.
            `lh` follows whatever font size the form's field class sets. */}
        <span className="min-w-0 flex-1 truncate min-h-[1lh]">{shown}</span>
        <ChevronDown aria-hidden className="size-3.5 shrink-0 text-slate-400" />
      </button>

      {open ? (
        <DropdownMenu
          menuId={menuId}
          rows={rows}
          value={value}
          label={label ?? shown}
          onPick={(picked) => {
            onChange(picked);
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
 *  Its own component so it mounts and unmounts with the menu, which is what puts it on
 *  the modal stack for exactly as long as it is up: Escape closes the menu, and the
 *  dialog underneath keeps its own Escape for when the menu is shut. */
function DropdownMenu({
  menuId,
  rows,
  value,
  label,
  onPick,
  onDismiss,
}: {
  menuId: string;
  rows: DropdownRow[];
  value: string;
  label: string;
  onPick: (picked: string) => void;
  onDismiss: () => void;
}) {
  const list = useRef<HTMLUListElement | null>(null);
  // Pickable rows only: arrows skip headers and disabled options, and Enter can only
  // ever commit something the caller would accept.
  const pickable = rows.filter((r) => isOption(r) && !r.disabled);
  // Starts on the current value, so opening and pressing Enter changes nothing.
  const [active, setActive] = useState(() =>
    Math.max(
      0,
      pickable.findIndex((r) => isOption(r) && r.value === value),
    ),
  );

  useModalEscape(onDismiss);
  useEffect(() => list.current?.focus(), []);

  return (
    <ul
      ref={list}
      role="listbox"
      tabIndex={-1}
      aria-label={label}
      aria-activedescendant={`${menuId}-${active}`}
      onKeyDown={(e) => {
        if (e.key === "ArrowDown" || e.key === "ArrowUp") {
          e.preventDefault();
          const step = e.key === "ArrowDown" ? 1 : -1;
          setActive((i) => (i + step + pickable.length) % pickable.length);
        }
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          const row = pickable[active];
          if (row && isOption(row)) onPick(row.value);
        }
        // Tab leaves the field, so it commits nothing and shuts the menu rather than
        // moving focus into a list the operator has already stepped past.
        if (e.key === "Tab") onDismiss();
      }}
      className="absolute z-10 mt-1 max-h-64 w-full overflow-y-auto rounded-md border border-slate-200 bg-white py-1 shadow-lg outline-none dark:border-slate-600 dark:bg-slate-800"
    >
      {rows.map((row, i) =>
        isOption(row) ? (
          <li
            key={row.value || `option-${i}`}
            id={`${menuId}-${pickable.indexOf(row)}`}
            role="option"
            aria-selected={row.value === value}
            aria-disabled={row.disabled || undefined}
            onPointerDown={(e) => {
              // The document listener above closes on any pointerdown outside; this one
              // is inside, and the default would move focus off the list before picking.
              e.preventDefault();
              if (!row.disabled) onPick(row.value);
            }}
            onPointerEnter={() => {
              const p = pickable.indexOf(row);
              if (p !== -1) setActive(p);
            }}
            className={`flex cursor-pointer items-center gap-2 px-3 py-1.5 text-sm ${
              !row.disabled && pickable[active] === row
                ? "bg-slate-100 dark:bg-slate-700"
                : ""
            } ${
              row.disabled
                ? "cursor-default text-slate-400 dark:text-slate-500"
                : row.value === value
                  ? "font-medium text-slate-900 dark:text-slate-100"
                  : "text-slate-600 dark:text-slate-300"
            }`}
          >
            <span className="min-w-0 flex-1 truncate">{row.label}</span>
          </li>
        ) : (
          <li
            key={`header-${i}`}
            aria-hidden
            className="px-3 pb-0.5 pt-1.5 text-[10px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500"
          >
            {row.header}
          </li>
        ),
      )}
    </ul>
  );
}
