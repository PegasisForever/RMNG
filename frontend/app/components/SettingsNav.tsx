// The settings panel's category rail: the seven groups every settings section is filed under,
// and the control that moves between them. A column down the left of a desktop-width panel, a
// horizontally scrolling strip above the pane on a phone.
//
// The categories are data rather than markup, so the panel, the panes and the stories all read
// one list, and hiding a category (a page with no board) is a filter instead of a branch.
import { Bot, Boxes, Container, LayoutGrid, Server, Sparkles, Terminal } from "lucide-react";
import type { LucideIcon } from "lucide-react";

export type SettingsCategory =
  | "board"
  | "agents"
  | "presets"
  | "claude"
  | "codex"
  | "clones"
  | "server";

export interface SettingsCategoryInfo {
  id: SettingsCategory;
  label: string;
  icon: LucideIcon;
}

/** Every category, in rail order. Roughly outside-in: what the operator arranges daily first,
 *  what the server was installed with last. */
export const SETTINGS_CATEGORIES: SettingsCategoryInfo[] = [
  { id: "board", label: "Board", icon: LayoutGrid },
  { id: "agents", label: "Agents", icon: Bot },
  { id: "presets", label: "Presets", icon: Boxes },
  { id: "claude", label: "Claude", icon: Sparkles },
  { id: "codex", label: "Codex", icon: Terminal },
  { id: "clones", label: "Clones", icon: Container },
  { id: "server", label: "Server", icon: Server },
];

export function SettingsNav({
  categories,
  active,
  onSelect,
}: {
  /** The categories to offer, already filtered. */
  categories: SettingsCategoryInfo[];
  active: SettingsCategory;
  onSelect: (category: SettingsCategory) => void;
}) {
  return (
    <nav
      aria-label="Settings categories"
      className="flex shrink-0 gap-1 overflow-x-auto border-b border-slate-100 p-2 dark:border-slate-800 sm:w-44 sm:flex-col sm:overflow-x-visible sm:overflow-y-auto sm:border-b-0 sm:border-r"
    >
      {categories.map(({ id, label, icon: Icon }) => {
        const current = id === active;
        return (
          <button
            key={id}
            type="button"
            onClick={() => onSelect(id)}
            // `page` rather than `true`: the rail swaps the panel's contents, which is the
            // same move a nav link makes.
            aria-current={current ? "page" : undefined}
            className={`flex shrink-0 items-center gap-2 rounded-lg px-2.5 py-1.5 text-left text-sm ${
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
