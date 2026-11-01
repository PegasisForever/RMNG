// The /ng Settings page: the dashboard's settings, every category and field, laid out for the
// keyboard's space. The categories and Save are a column on the left, so the pane keeps the
// whole height; it scrolls on its own. On a narrow page (a phone upright) the categories are
// icons only, so the pane keeps the width. The panes are the dashboard's (`SettingsPane`).
//
// Pure: the form and what it can do come from `useSettingsPanel`, as on the desktop.
import { Save } from "lucide-react";

import { SettingsPane, settingsCategories, type SettingsPanelViewProps } from "~/components/SettingsPanelView";

import { EmptyLine, ErrorLine, LINE, LIT, PRESS, TEXT_2 } from "./ui";

export type SettingsViewProps = Omit<SettingsPanelViewProps, "onClose">;

export function SettingsView(p: SettingsViewProps) {
  const categories = settingsCategories(p.boardColumns);
  const active = categories.find((c) => c.id === p.category) ?? categories[0];
  return (
    <div className="@container flex h-full min-h-0">
      <nav aria-label="Settings categories" className={`flex w-12 shrink-0 flex-col gap-1 border-r p-1.5 @lg:w-32 ${LINE}`}>
        {categories.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            type="button"
            onClick={() => p.onCategoryChange(id)}
            aria-current={id === active.id ? "page" : undefined}
            aria-label={label}
            title={label}
            className={`flex h-9 items-center justify-center gap-2 rounded-md px-2 text-left text-sm @lg:justify-start ${
              id === active.id ? `${LIT} font-medium` : `${TEXT_2} ${PRESS}`
            }`}
          >
            <Icon aria-hidden className="size-4 shrink-0" />
            <span className="hidden truncate @lg:inline">{label}</span>
          </button>
        ))}
        <span className="flex-1" />
        {p.saved ? <span className="text-center text-xs font-medium text-emerald-700 dark:text-emerald-300">Saved</span> : null}
        <button
          type="button"
          onClick={p.onSave}
          disabled={!p.draft || p.saving}
          aria-label="Save"
          className="flex h-9 items-center justify-center gap-1.5 rounded-md bg-emerald-600 text-sm font-medium text-white active:bg-emerald-500 disabled:opacity-40"
        >
          <Save aria-hidden className="size-4" />
          <span className="hidden @lg:inline">{p.saving ? "Saving…" : "Save"}</span>
        </button>
      </nav>
      <div className="flex min-w-0 flex-1 flex-col">
        {p.error ? <ErrorLine message={p.error} /> : null}
        <div className="min-h-0 flex-1 space-y-4 overflow-y-auto overscroll-contain p-3 [&>section:first-child]:border-t-0 [&>section:first-child]:pt-0">
          {p.draft ? <SettingsPane {...p} draft={p.draft} category={active.id} /> : <EmptyLine>Loading…</EmptyLine>}
        </div>
      </div>
    </div>
  );
}
