// The parts every /ng page is made of. A page has a small space (a phone's, beside the clone's
// screen), so each one is a 36 px header over a body that scrolls, and every control is a
// finger's size: nothing here waits for a hover, and a menu is a sheet of large tiles over the
// page (`ActionSheet`), not a small list at a pointer.
//
// The pages are flat content (`../GLASS.md`): on the glass of their panel over the clone's
// screen, or on the solid ground of their own window. Each color has its light value and its
// `dark:` one; the names below are the ones the pages share.
import { Check, ChevronLeft, type LucideIcon, X } from "lucide-react";
import {
  createContext,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";

import { type Box, MenuDivider, MenuItem, MenuPanel } from "~/components/OverflowMenu";

/** The pages' panel over the clone's screen (narrow): glass with no blur or shadow of its own,
 *  as the wide pages' columns are (`ng-glass-flat`): the backdrop under the pages
 *  (`PAGES_BACKDROP`) blurs and darkens the screen for them. */
export const PAGES_GLASS = "ng-glass ng-glass-thick ng-glass-flat rounded-2xl";

/** A column of the wide pages (`./WideLayout.tsx`) on its own: over the clone's screen, glass
 *  with no blur or shadow of its own (`ng-glass-flat`: the backdrop under the pages blurs and
 *  darkens the screen for them all); a flat hollow on the solid ground of the pages' window. */
export const COLUMN_GLASS = "ng-glass ng-glass-thick ng-glass-flat rounded-2xl overflow-hidden";

/** Over the whole clone's screen while the pages are open, under them and the floating buttons:
 *  it blurs and darkens the screen. It takes no touch or click. */
export const PAGES_BACKDROP = "pointer-events-none bg-black/25 backdrop-blur-md dark:bg-black/40";
export const COLUMN_SOLID =
  "overflow-hidden rounded-xl border border-black/10 bg-black/[0.03] dark:border-white/10 dark:bg-white/[0.03]";
const ColumnFrame = createContext(COLUMN_SOLID);
/** The wide layout says which frame its columns wear. */
export const ColumnFrameProvider = ColumnFrame.Provider;
export const useColumnFrame = () => useContext(ColumnFrame);

/** A card in a column (a clone, a ticket): its edge, and its fill (white on the column, light on
 *  a dark one) when nothing else fills it. */
export const CARD = "rounded-lg border border-black/10 dark:border-white/10";
export const CARD_FILL = "bg-white dark:bg-white/[0.06]";

/** Main text, secondary text, the least text. */
export const TEXT = "text-neutral-900 dark:text-white";
export const TEXT_2 = "text-black/60 dark:text-white/60";
export const TEXT_3 = "text-black/40 dark:text-white/40";
/** A line round a box, and the lines between the rows of a list (with `divide-y`). */
export const LINE = "border-black/10 dark:border-white/10";
export const DIVIDE = "divide-black/10 dark:divide-white/10";
/** A row or a tile under the mouse (Tailwind's `hover:` is for a mouse only), or a finger. */
export const PRESS = "hover:bg-black/5 active:bg-black/10 dark:hover:bg-white/5 dark:active:bg-white/10";
/** A box on the page (a card, a field's ground, a hollow). */
export const BOX = "bg-white/60 dark:bg-white/[0.06]";
/** A button: a small box of its own. */
export const BUTTON = `bg-white/60 border border-black/10 dark:bg-white/10 dark:border-white/10 ${TEXT} active:bg-black/10 dark:active:bg-white/20`;
/** The one thing on: the other look (as lit glass). */
export const LIT = "bg-neutral-900 text-white dark:bg-white dark:text-neutral-900";
/** The page's own row, lit: the clone on the screen. */
export const HERE = "bg-emerald-500/10 shadow-[inset_3px_0_0_var(--color-emerald-500)] dark:bg-emerald-400/10 dark:shadow-[inset_3px_0_0_var(--color-emerald-400)]";

/** A page: its header, an error line when there is one, and the body. The body scrolls unless
 *  `fill` (the page lays out its own scrolling, as the chat does). */
export function Page({
  header,
  error,
  onDismissError,
  fill = false,
  noScrollbar = false,
  children,
}: {
  header?: ReactNode;
  error?: string | null;
  onDismissError?: () => void;
  fill?: boolean;
  /** It scrolls with no bar showing (a narrow column, where the bar takes room). */
  noScrollbar?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="flex h-full min-h-0 flex-col">
      {header}
      {error ? <ErrorLine message={error} onDismiss={onDismissError} /> : null}
      <div
        className={`min-h-0 flex-1 ${fill ? "flex flex-col overflow-hidden" : "overflow-y-auto overscroll-y-contain"} ${
          noScrollbar ? "[scrollbar-width:none] [&::-webkit-scrollbar]:hidden" : ""
        }`}
      >
        {children}
      </div>
    </div>
  );
}

/** The header row: a back button when there is somewhere to go back to, the title (with a
 *  second line in grey), and the page's buttons on the right. */
export function PageHeader({
  title,
  sub,
  onBack,
  children,
}: {
  title: ReactNode;
  sub?: ReactNode;
  onBack?: () => void;
  /** The page's buttons. */
  children?: ReactNode;
}) {
  return (
    <div className={`flex h-9 shrink-0 items-center gap-1 border-b pr-1 ${LINE}`}>
      {onBack ? (
        <button
          type="button"
          onClick={onBack}
          aria-label="Back"
          className={`flex h-9 w-9 shrink-0 items-center justify-center ${TEXT_2} ${PRESS}`}
        >
          <ChevronLeft aria-hidden className="size-5" />
        </button>
      ) : (
        <span className="w-2 shrink-0" />
      )}
      <div className="flex min-w-0 flex-1 items-baseline gap-2">
        <h2 className={`truncate text-sm font-semibold ${TEXT}`}>{title}</h2>
        {sub ? <span className={`min-w-0 truncate text-xs ${TEXT_3}`}>{sub}</span> : null}
      </div>
      {children}
    </div>
  );
}

/** A button in a header, or anywhere a small one goes: an icon, a label, or both. */
export function HeaderButton({
  icon: Icon,
  label,
  showLabel = false,
  primary = false,
  disabled = false,
  onClick,
}: {
  icon?: LucideIcon;
  label: string;
  /** The label shows beside the icon; otherwise it is only for screen readers. */
  showLabel?: boolean;
  primary?: boolean;
  disabled?: boolean;
  onClick: (e: ReactMouseEvent<HTMLButtonElement>) => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-label={label}
      className={`flex h-7 min-w-8 shrink-0 items-center justify-center gap-1 rounded-md px-2 text-xs font-medium disabled:opacity-40 ${
        primary ? "bg-emerald-600 text-white active:bg-emerald-500" : BUTTON
      }`}
    >
      {Icon ? <Icon aria-hidden className="size-4" /> : null}
      {showLabel || !Icon ? <span>{label}</span> : null}
    </button>
  );
}

/** The last action that failed, in a red line under the header. */
export function ErrorLine({ message, onDismiss }: { message: string; onDismiss?: () => void }) {
  return (
    <p
      role="alert"
      className="flex shrink-0 items-start gap-2 bg-rose-500/15 px-3 py-1.5 text-xs text-rose-700 dark:text-rose-300"
    >
      <span className="min-w-0 flex-1 break-words">{message}</span>
      {onDismiss ? (
        <button type="button" onClick={onDismiss} aria-label="Dismiss" className="-my-1 p-1">
          <X aria-hidden className="size-3.5" />
        </button>
      ) : null}
    </p>
  );
}

/** A grey line where a list has nothing to show. */
export function EmptyLine({ children }: { children: ReactNode }) {
  return <p className={`px-4 py-8 text-center text-sm ${TEXT_3}`}>{children}</p>;
}

/** A row of choices, one of them on. */
export function Segmented<T extends string>({
  options,
  value,
  onChange,
}: {
  options: { value: T; label: string; badge?: ReactNode }[];
  value: T;
  onChange: (value: T) => void;
}) {
  return (
    <div role="tablist" className="flex shrink-0 gap-0.5 rounded-md bg-black/5 p-0.5 dark:bg-white/10">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="tab"
          aria-selected={o.value === value}
          onClick={() => onChange(o.value)}
          className={`flex h-6 items-center gap-1 rounded px-2.5 text-xs font-medium ${
            o.value === value ? LIT : `${TEXT_2} ${PRESS}`
          }`}
        >
          {o.label}
          {o.badge}
        </button>
      ))}
    </div>
  );
}

/** One thing a sheet offers. `onSelect` may answer a promise: a copy says whether it landed,
 *  and the tile shows "Copied" before the sheet closes. */
export type SheetAction = {
  icon: LucideIcon;
  label: string;
  onSelect: () => void | Promise<boolean>;
  danger?: boolean;
  /** Ticked: the choice that is on now (a column the clone is in). */
  current?: boolean;
};

/** A group of tiles in a sheet, under a small heading when it has one. */
export type SheetGroup = { title?: string; actions: SheetAction[] };

/** What a ⋯ button opens on a touch screen: a sheet over the page, with a tile per action, in
 *  groups. A tap on a tile does it and closes the sheet; a tap beside the sheet closes it. */
export function ActionSheet({
  title,
  groups,
  onClose,
}: {
  title: ReactNode;
  groups: SheetGroup[];
  onClose: () => void;
}) {
  const [copied, setCopied] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const select = (a: SheetAction) => {
    const answer = a.onSelect();
    if (!answer) return onClose();
    void answer.then((ok) => {
      setCopied(ok ? a.label : null);
      timer.current = setTimeout(onClose, ok ? 700 : 0);
    });
  };
  return (
    // Over the whole panel (its glass is the containing block of what is fixed in it), or the
    // whole window. The sheet is solid: on glass, over a dimmed page, glass would look murky.
    <div className="fixed inset-0 z-40 flex flex-col items-center justify-end bg-black/25 dark:bg-black/50" onClick={onClose}>
      <div
        role="menu"
        aria-label={typeof title === "string" ? title : undefined}
        className="max-h-full w-full max-w-xl overflow-y-auto overscroll-y-contain rounded-t-2xl bg-neutral-100 px-2 pb-2 shadow-[0_-8px_32px_rgb(0_0_0/0.25)] dark:bg-neutral-900"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="sticky top-0 flex h-10 items-center gap-2 bg-neutral-100 pl-1 dark:bg-neutral-900">
          <div className={`min-w-0 flex-1 truncate text-sm font-semibold ${TEXT}`}>{title}</div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className={`flex h-8 w-8 items-center justify-center rounded-md ${TEXT_2} ${PRESS}`}
          >
            <X aria-hidden className="size-4" />
          </button>
        </div>
        {groups
          .filter((g) => g.actions.length > 0)
          .map((g, i) => (
            <div key={i} className="mt-1 first-of-type:mt-0">
              {g.title ? (
                <div className={`px-1 pb-1 pt-1 text-[11px] font-medium ${TEXT_3}`}>{g.title}</div>
              ) : null}
              <div className="grid grid-cols-4 gap-1.5">
                {g.actions.map((a) => (
                  <button
                    key={a.label}
                    type="button"
                    role="menuitem"
                    onClick={() => select(a)}
                    className={`flex h-14 min-w-0 flex-col items-center justify-center gap-1 rounded-lg px-1 text-center text-xs leading-tight ${
                      a.current
                        ? LIT
                        : a.danger
                          ? "bg-rose-500/10 text-rose-600 active:bg-rose-500/20 dark:text-rose-400"
                          : `bg-white text-neutral-900 active:bg-neutral-200 dark:bg-white/10 dark:text-white dark:active:bg-white/20`
                    }`}
                  >
                    <a.icon aria-hidden className="size-4 shrink-0" />
                    <span className="line-clamp-2 break-words">{copied === a.label ? "Copied" : a.label}</span>
                  </button>
                ))}
              </div>
            </div>
          ))}
      </div>
    </div>
  );
}

/** A click from a mouse (or the keyboard), not a finger or a pen: it opens a menu as the
 *  dashboard's ⋮ does (`DesktopMenu`), not a sheet of large tiles. */
export function isMouseClick(e: ReactMouseEvent) {
  const kind = (e.nativeEvent as PointerEvent).pointerType;
  return kind !== "touch" && kind !== "pen";
}

/** The box of what was clicked, for a menu to hang off it. */
export function boxOf(e: ReactMouseEvent): Box {
  const r = e.currentTarget.getBoundingClientRect();
  return { top: r.top, bottom: r.bottom, left: r.left, right: r.right };
}

/** The same groups as an `ActionSheet`, as the dashboard's ⋮ menu draws them, off `at`. */
export function DesktopMenu({ at, groups, onClose }: { at: Box; groups: SheetGroup[]; onClose: () => void }) {
  const shown = groups.filter((g) => g.actions.length > 0);
  return (
    <MenuPanel at={at} onClose={onClose}>
      {shown.map((g, i) => (
        <div key={i}>
          {i > 0 ? <MenuDivider /> : null}
          {g.title ? <p className="px-3 pb-0.5 pt-1 text-[11px] font-medium text-slate-400 dark:text-slate-500">{g.title}</p> : null}
          {g.actions.map((a) =>
            a.current ? (
              <p key={a.label} className="flex w-full items-center gap-1.5 px-3 py-1.5 text-xs text-slate-400 dark:text-slate-500">
                <a.icon aria-hidden className="size-4 shrink-0" />
                <span className="flex-1">{a.label}</span>
                <Check aria-hidden className="size-3.5" />
              </p>
            ) : (
              <MenuItem key={a.label} icon={a.icon} label={a.label} danger={a.danger} onClick={() => void a.onSelect()} />
            ),
          )}
        </div>
      ))}
    </MenuPanel>
  );
}

/** Stops a press from starting a drag of the card it is in (a mouse's, a finger's). */
export const NO_DRAG = {
  onMouseDown: (e: ReactMouseEvent) => e.stopPropagation(),
  onTouchStart: (e: { stopPropagation: () => void }) => e.stopPropagation(),
};
