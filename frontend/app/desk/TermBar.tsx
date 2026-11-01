// The tab bar over a headless clone's terminal (`./TerminalScreen.tsx`): one tab per tmux
// session and a "+" that makes a new one. It is as tall as the floating buttons' row, and keeps
// its right end free for them: they float over it, not over the terminal's lines. The terminal
// is dark in both looks, so the bar is too: flat content (`./GLASS.md`) on a solid panel.
import { Plus } from "lucide-react";
import type { ReactNode } from "react";

import { FLOAT_BUTTON, FLOAT_GAP } from "./FloatingButtons";

/** The bar's height under the clock bar, CSS px: the floating buttons' row. */
export const TERM_BAR_H = FLOAT_GAP * 2 + FLOAT_BUTTON;
/** Room kept at its right for the floating buttons (as many as five), CSS px. */
const BUTTONS_ROOM = FLOAT_GAP + 5 * (FLOAT_BUTTON + FLOAT_GAP);

export function TermBar({
  sessions,
  active,
  onPick,
  onNew,
  top,
}: {
  sessions: string[];
  active: string | null;
  onPick: (session: string) => void;
  onNew: () => void;
  /** The clock bar's safe-area inset, CSS px. */
  top: number;
}) {
  return (
    <div
      className="flex shrink-0 items-center gap-1 overflow-x-auto bg-neutral-950 pl-2"
      style={{ height: top + TERM_BAR_H, paddingTop: top, paddingRight: BUTTONS_ROOM }}
    >
      {sessions.map((s) => (
        <Tab key={s} on={s === active} onClick={() => onPick(s)}>
          {s}
        </Tab>
      ))}
      <Tab on={false} onClick={onNew} label="New tmux session">
        <Plus aria-hidden className="size-4" />
      </Tab>
    </div>
  );
}

function Tab({ on, onClick, label, children }: { on: boolean; onClick: () => void; label?: string; children: ReactNode }) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={label ? undefined : on}
      title={label}
      // The keys stay with the terminal
      onMouseDown={(e) => e.preventDefault()}
      onClick={onClick}
      className={`flex h-9 shrink-0 items-center rounded-lg px-3 font-mono text-sm transition-colors duration-150 ${
        on ? "bg-white/15 text-white" : "text-white/60 hover:bg-white/10 hover:text-white"
      }`}
    >
      {children}
    </button>
  );
}
