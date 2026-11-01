import { ExternalLink, Settings } from "lucide-react";
import { type PointerEvent, type ReactNode } from "react";

import { GAP, TOOL_H } from "./geometry";

/** The control panel the tool row opens, in place of the keyboard. */
export type PanelTab = "usage" | "tickets" | "clones" | "notes" | "assistant" | "settings";

/** Each tab: its button's label (the settings tab shows a gear) and its page's name. */
export const TABS: { tab: PanelTab; label: string; page: string }[] = [
  { tab: "usage", label: "Usage", page: "Account usage" },
  { tab: "tickets", label: "Tickets", page: "Tickets" },
  { tab: "clones", label: "Clones", page: "Other clones" },
  { tab: "notes", label: "Notes", page: "Notes" },
  { tab: "assistant", label: "Assistant", page: "Assistant" },
  { tab: "settings", label: "Settings", page: "Settings" },
];

/** The server's CPU, memory and disk, as the dashboard shows them (`formatLxcUsage`). */
export type ServerUsage = { cpu: string; mem: string; disk: string };

/** Narrower than this (CSS px), the row has no room for the server's numbers: the Usage page
 *  shows them. */
export const ROW_STATS_MIN = 520;

type Props = {
  /** The row's width, CSS px. */
  width: number;
  /** Null until the server's first numbers arrive. */
  usage: ServerUsage | null;
  /** The open tab; null while the keyboard shows. */
  tab: PanelTab | null;
  /** A tab was tapped: the container opens it, or closes it when it is open. */
  onTab: (tab: PanelTab) => void;
  /** A count on a tab's button. */
  badges?: Partial<Record<PanelTab, number>>;
  /** Move the control panel to a window of its own; absent where the browser cannot open one. */
  onPopOut?: () => void;
};

/** A box on the row (a number, a tab), flat on the control panel's surface (`./GLASS.md`). */
const CHIP = "rounded-[10px] border border-black/10 bg-white/50 dark:border-white/10 dark:bg-white/[0.06]";
/** A tab or a button on the row, under the mouse. */
const IDLE = `${CHIP} text-neutral-900 hover:bg-white/80 dark:text-white dark:hover:bg-white/10`;
/** The open tab: the other look, as lit glass. */
const ON = "rounded-[10px] border border-transparent bg-neutral-900 text-white dark:bg-white dark:text-neutral-900";

/** The /ng tool row, over the control panel: the server's CPU, memory and disk (where there is room),
 *  then tabs that act as tabs do. One is open at most. */
export function ToolRow({ width, usage, tab, onTab, badges, onPopOut }: Props) {
  return (
    <div className="flex shrink-0" style={{ height: TOOL_H, gap: GAP }}>
      {width >= ROW_STATS_MIN ? (
        <>
          <Stat label="CPU" value={usage?.cpu} />
          <Stat label="Mem" value={usage?.mem} />
          <Stat label="Disk" value={usage?.disk} />
        </>
      ) : null}
      {TABS.map((t) => (
        <TabButton
          key={t.tab}
          active={tab === t.tab}
          label={t.label}
          narrow={t.tab === "settings"}
          onTap={() => onTab(t.tab)}
        >
          {t.tab === "settings" ? <Settings size={18} strokeWidth={1.75} aria-hidden /> : t.label}
          {badges?.[t.tab] ? (
            <span className="absolute -right-1 -top-1 flex h-4 min-w-4 items-center justify-center rounded-full bg-rose-500 px-1 text-[10px] font-semibold leading-none text-white shadow-[inset_0_1px_0_rgb(255_255_255/0.4),0_1px_2px_rgb(0_0_0/0.45)]">
              {badges[t.tab]}
            </span>
          ) : null}
        </TabButton>
      ))}
      {onPopOut ? <PopOutButton onPopOut={onPopOut} /> : null}
    </div>
  );
}

/** Moves the control panel to a window of its own. */
export function PopOutButton({ onPopOut }: { onPopOut: () => void }) {
  return (
    <button
      type="button"
      aria-label="Open the control panel in a window of its own"
      title="Open the control panel in a window of its own"
      onClick={onPopOut}
      className={`flex size-9 shrink-0 items-center justify-center ${IDLE}`}
    >
      <ExternalLink size={16} strokeWidth={1.75} aria-hidden />
    </button>
  );
}

function Stat({ label, value }: { label: string; value: string | undefined }) {
  return (
    <div className={`flex min-w-[58px] shrink-0 flex-col items-center justify-center px-1.5 leading-none ${CHIP}`}>
      <span className="text-[10px] text-black/45 dark:text-white/45">{label}</span>
      <span className="mt-0.5 font-mono text-xs tabular-nums text-neutral-900 dark:text-white">{value ?? "—"}</span>
    </div>
  );
}

function TabButton({
  active,
  label,
  narrow,
  onTap,
  children,
}: {
  active: boolean;
  label: string;
  /** An icon only: as wide as it is tall. */
  narrow: boolean;
  onTap: () => void;
  children: ReactNode;
}) {
  return (
    <div
      role="tab"
      aria-label={label}
      aria-selected={active}
      className={`relative flex min-w-0 cursor-pointer items-center justify-center text-sm transition-colors duration-150 ${
        narrow ? "w-9 shrink-0" : "flex-1"
      } ${active ? ON : IDLE}`}
      onPointerDown={(e: PointerEvent<HTMLDivElement>) => {
        if (e.pointerType === "mouse") e.currentTarget.setPointerCapture(e.pointerId);
        onTap();
      }}
    >
      {children}
    </div>
  );
}
