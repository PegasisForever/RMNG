// The clone dialog's tab strip: which kind of clone this is going to be. The three tabs are
// three different requests, not three views of one — the fields below change, and so does the
// server route the Clone button ends up hitting.
import type { CloneMode } from "~/lib/cloneDraft";

const TABS: { mode: CloneMode; label: string }[] = [
  { mode: "existing", label: "Existing ticket" },
  { mode: "create", label: "New ticket" },
  { mode: "plain", label: "No ticket" },
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
    <div className="flex gap-0.5 rounded-md bg-slate-100 p-0.5 text-xs font-medium dark:bg-slate-800">
      {TABS.map((tab) => (
        <button
          key={tab.mode}
          type="button"
          disabled={disabled}
          onClick={() => onModeChange(tab.mode)}
          className={`flex-1 rounded px-2 py-1 disabled:opacity-50 ${
            mode === tab.mode
              ? "bg-white text-slate-900 shadow-sm dark:bg-slate-700 dark:text-slate-100"
              : "text-slate-500 hover:text-slate-700 dark:text-slate-400 dark:hover:text-slate-200"
          }`}
        >
          {tab.label}
        </button>
      ))}
    </div>
  );
}
