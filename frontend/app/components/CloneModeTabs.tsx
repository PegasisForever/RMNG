// The clone dialog's left rail: which kind of clone this is going to be. The four entries
// are four different requests, not four views of one — the fields beside them change, and
// so does the server route the Create button ends up hitting. The first three fork the
// picked source clone; the fourth creates from a template image onto a fresh empty home.
//
// Same look as the settings rail (a strip across the top on phones, a column beside the
// form past `sm`), but its own component: the entries are dialog modes, not settings
// categories, and sharing the component would weld the two dialogs together.
import { Boxes, FilePlus, MessageSquare, Ticket } from "lucide-react";
import type { LucideIcon } from "lucide-react";

import type { CloneMode } from "~/lib/cloneDraft";

const TABS: { mode: CloneMode; label: string; icon: LucideIcon }[] = [
  { mode: "existing", label: "Existing ticket", icon: Ticket },
  { mode: "create", label: "New ticket", icon: FilePlus },
  { mode: "plain", label: "No ticket", icon: MessageSquare },
  { mode: "template", label: "From template", icon: Boxes },
];

export function CloneModeTabs({
  mode,
  disabled = false,
  onModeChange,
}: {
  mode: CloneMode;
  /** Locked once a clone has been started. Switching tabs mid-operation would rebuild the
   *  form under an in-flight request. */
  disabled?: boolean;
  onModeChange: (mode: CloneMode) => void;
}) {
  return (
    <nav
      aria-label="Clone modes"
      className="flex shrink-0 gap-1 overflow-x-auto border-b border-slate-100 p-2 dark:border-slate-800 sm:w-44 sm:flex-col sm:overflow-x-visible sm:overflow-y-auto sm:border-b-0 sm:border-r"
    >
      {TABS.map(({ mode: m, label, icon: Icon }) => {
        const current = m === mode;
        return (
          <button
            key={m}
            type="button"
            disabled={disabled}
            onClick={() => onModeChange(m)}
            aria-current={current ? "page" : undefined}
            className={`flex shrink-0 items-center gap-2 rounded-lg px-2.5 py-1.5 text-left text-sm disabled:opacity-50 ${
              current
                ? "bg-slate-100 font-medium text-slate-900 dark:bg-slate-700 dark:text-slate-100"
                : "text-slate-500 hover:bg-slate-50 hover:text-slate-700 dark:text-slate-400 dark:hover:bg-slate-700/50 dark:hover:text-slate-200"
            }`}
          >
            <Icon className="size-4 shrink-0" />
            {label}
          </button>
        );
      })}
    </nav>
  );
}
