// One clone on a phone: a back chevron, its status, and a two-tab switch between the agent
// chat and the notes. The active tab owns the whole screen below the header, because on a
// phone there is no second column to put the other one in.
//
// Pure, like AppShellV2: the notes and chat panes arrive as slots because both real
// implementations are lazy-loaded and client-only, so a story can fill them with fixtures.
// The page fills its container rather than the viewport, so the route wraps it in `100dvh`
// and the composer inside the chat slot is what deals with the keyboard.
import { ChevronLeft } from "lucide-react";
import type { ReactNode } from "react";

import { CloneStatusDot, statusLabel } from "~/components/mobile/CloneStatus";
import type { CloneTicket } from "~/lib/tickets";
import type { Clone } from "~/lib/types";

/** Which pane owns the screen. There is no third: settings, archiving, and reordering are
 *  desktop-only, so nothing else competes for the space. */
export type CloneTab = "chat" | "notes";

/** The two panes, in order. The second one names itself, because it holds the clone's notes
 *  or the ticket it was made from and the tab is what says which. */
function tabsFor(notesLabel: string): { id: CloneTab; label: string }[] {
  return [
    { id: "chat", label: "Chat" },
    { id: "notes", label: notesLabel },
  ];
}

export interface MobileCloneProps {
  clone: Clone;
  /** This clone's Linear ticket as Linear has it now. Absent ⇒ the header draws the title the
   *  clone stored when it was made. */
  ticket?: CloneTicket;
  tab: CloneTab;
  onTabChange: (tab: CloneTab) => void;
  /** Back to the clone list. */
  onBack: () => void;
  /** The notes editor for this clone, or its ticket when it has one. */
  notes: ReactNode;
  /** What the second tab is called. The default names the notes; a clone showing its ticket
   *  there passes "Ticket". */
  notesLabel?: string;
  /** The agent chat for this clone. */
  chat: ReactNode;
  /** Last failed action, shown as a banner under the tabs. */
  error?: string | null;
}

export function MobileClone({
  clone,
  ticket,
  tab,
  onTabChange,
  onBack,
  notes,
  notesLabel = "Notes",
  chat,
  error = null,
}: MobileCloneProps) {
  return (
    <div className="flex h-full min-h-0 flex-col bg-white dark:bg-slate-950">
      <header className="flex shrink-0 items-center gap-1 border-b border-slate-200 bg-white px-2 pb-2 pt-[max(0.75rem,env(safe-area-inset-top))] dark:border-slate-800 dark:bg-slate-900">
        <button
          type="button"
          onClick={onBack}
          aria-label="Back to clones"
          className="flex size-11 shrink-0 items-center justify-center rounded-full text-slate-500 active:bg-slate-100 dark:text-slate-400 dark:active:bg-slate-800"
        >
          <ChevronLeft aria-hidden className="size-5" />
        </button>
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-sm font-semibold text-slate-800 dark:text-slate-100">
            {ticket?.title ?? clone.displayName ?? clone.id}
          </h1>
          <p className="flex items-center gap-1.5 text-xs text-slate-500 dark:text-slate-400">
            <CloneStatusDot clone={clone} />
            <span className="truncate">
              {statusLabel(clone)}
              {clone.linearTicket ? ` · ${clone.linearTicket}` : ""}
            </span>
          </p>
        </div>
      </header>

      <div
        role="tablist"
        aria-label="Clone panes"
        className="grid shrink-0 grid-cols-2 border-b border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-900"
      >
        {tabsFor(notesLabel).map((t) => {
          const active = t.id === tab;
          return (
            <button
              key={t.id}
              type="button"
              role="tab"
              aria-selected={active}
              onClick={() => onTabChange(t.id)}
              className={`min-h-11 border-b-2 text-sm font-medium ${
                active
                  ? "border-emerald-600 text-emerald-700 dark:text-emerald-400"
                  : "border-transparent text-slate-500 active:bg-slate-100 dark:text-slate-400 dark:active:bg-slate-800"
              }`}
            >
              {t.label}
            </button>
          );
        })}
      </div>

      {error ? (
        <p
          role="alert"
          className="shrink-0 bg-rose-50 px-4 py-2 text-sm text-rose-700 dark:bg-rose-950 dark:text-rose-300"
        >
          {error}
        </p>
      ) : null}

      {/* Only the active pane renders. A phone shows one at a time, so keeping a hidden
          BlockNote editor mounted costs memory and a measure pass for nothing — the
          document itself lives in the caller, which is what makes the switch cheap.
          The two panes want opposite containers, the same way the desktop shell gives them
          opposite containers: the chat is a flex column, so its thread takes the slack and
          its composer sits on the bottom edge, while the notes are one long scroll. */}
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
        {tab === "chat" ? (
          chat
        ) : (
          <div className="min-h-0 flex-1 overflow-y-auto py-2">{notes}</div>
        )}
      </div>
    </div>
  );
}
