import { Settings } from "lucide-react";
import { type PointerEvent, type ReactNode } from "react";

import { Haptic } from "./Haptic";
import { SettingsPage } from "./SettingsPage";
import { GAP } from "./geometry";

/** The pages the tool row opens, in place of the keyboard. */
export type NgTab = "usage" | "tickets" | "clones" | "notes" | "settings";

/** Each tab: its button's label (the settings tab shows a gear) and its page's name. */
export const TABS: { tab: NgTab; label: string; page: string }[] = [
  { tab: "usage", label: "Usage", page: "Account usage" },
  { tab: "tickets", label: "Tickets", page: "Tickets" },
  { tab: "clones", label: "Clones", page: "Other clones" },
  { tab: "notes", label: "Notes", page: "Notes and assistant" },
  { tab: "settings", label: "Settings", page: "Settings" },
];

/** The server's CPU, memory and disk, as the dashboard shows them (`formatLxcUsage`). */
export type ServerUsage = { cpu: string; mem: string; disk: string };

type Props = {
  box: { left: number; top: number; width: number; height: number };
  /** Null until the server's first numbers arrive. */
  usage: ServerUsage | null;
  /** The open tab; null while the keyboard shows. */
  tab: NgTab | null;
  /** A tab was tapped: the container opens it, or closes it when it is open. */
  onTab: (tab: NgTab) => void;
};

/** The /ng tool row, under the screen: the server's CPU, memory and disk, then tabs that act
 *  as tabs do. One is open at most, and its page takes the keyboard's place. */
export function ToolRow({ box, usage, tab, onTab }: Props) {
  return (
    <div className="absolute flex" style={{ ...box, gap: GAP }}>
      <Stat label="CPU" value={usage?.cpu} />
      <Stat label="Mem" value={usage?.mem} />
      <Stat label="Disk" value={usage?.disk} />
      {TABS.map((t) => (
        <TabButton
          key={t.tab}
          active={tab === t.tab}
          label={t.label}
          narrow={t.tab === "settings"}
          onTap={() => onTab(t.tab)}
        >
          {t.tab === "settings" ? <Settings size={18} strokeWidth={1.75} aria-hidden /> : t.label}
        </TabButton>
      ))}
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string | undefined }) {
  return (
    <div className="flex min-w-[58px] shrink-0 flex-col items-center justify-center rounded-md bg-neutral-900 px-1.5 leading-none">
      <span className="text-[10px] text-neutral-500">{label}</span>
      <span className="mt-0.5 font-mono text-xs text-neutral-200 tabular-nums">{value ?? "—"}</span>
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
      className={`relative flex min-w-0 items-center justify-center rounded-md text-sm ${
        narrow ? "w-9 shrink-0" : "flex-1"
      } ${active ? "bg-neutral-200 text-black" : "bg-neutral-800 text-neutral-100"}`}
      onPointerDown={(e: PointerEvent<HTMLDivElement>) => {
        if (e.pointerType === "mouse") e.currentTarget.setPointerCapture(e.pointerId);
        onTap();
      }}
    >
      {children}
      <Haptic />
    </div>
  );
}

/** A tab's page. Those not made yet show their name in the middle. */
export function TabPage({ tab, box }: { tab: NgTab; box: Props["box"] }) {
  const page = TABS.find((t) => t.tab === tab)!.page;
  return (
    <div role="tabpanel" aria-label={page} className="absolute" style={box}>
      {tab === "settings" ? (
        <SettingsPage />
      ) : (
        <div className="flex h-full items-center justify-center text-sm text-neutral-500">{page}</div>
      )}
    </div>
  );
}
