// The clone dialog's Existing-ticket tab: paste a Linear link or a bare `WE-142`, and the
// dialog says what it made of it — the ticket id, the hostname the clone will get, and the
// preset the id's prefix resolved to.
//
// The resolved-preset line belongs to this tab and only this tab. New ticket does not need it
// (its team dropdown already reads "DEV · preset 1", i.e. the same choice) and No ticket picks
// a preset by hand, so neither one draws it.
import { cloneField, cloneLabel } from "~/components/cloneFieldStyles";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
import { workspaceBadge } from "~/lib/workspace";

/** What `parseTicketInput` makes of the pasted text. */
export interface ParsedTicket {
  identifier: string;
  prefix: string;
  hostname: string;
}

export function CloneExistingTicketFields({
  ticket,
  parsed,
  preset,
  presets,
  onTicketChange,
  onSubmit,
}: {
  ticket: string;
  /** The id parsed out of `ticket`, or null when there is none in it yet. */
  parsed: ParsedTicket | null;
  /** The preset the prefix resolved to. Undefined before a ticket parses, and also when no
   *  preset claims the prefix — which is a blocking state, not a cosmetic one. */
  preset: PresetRedacted | undefined;
  /** Every configured preset. Empty means none are configured at all, and the tab then says
   *  nothing about presets rather than blaming the operator for a missing prefix. */
  presets: PresetRedacted[];
  onTicketChange: (ticket: string) => void;
  /** Enter in the field starts the clone. */
  onSubmit: () => void;
}) {
  return (
    <>
      <label className={`mt-3 ${cloneLabel}`}>
        Linear ticket link or id
        <input
          autoFocus
          value={ticket}
          onChange={(e) => onTicketChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSubmit();
          }}
          placeholder="https://linear.app/…/issue/WE-142  or  WE-142"
          spellCheck={false}
          className={cloneField}
        />
        {ticket && !parsed ? (
          <p className="mt-1 text-[11px] text-red-600 dark:text-red-400">
            couldn’t find a ticket id (like WE-142) in that
          </p>
        ) : null}
        {parsed ? (
          <p className="mt-1.5 flex items-center gap-1.5 text-[11px] font-normal text-slate-500 dark:text-slate-400">
            <span className={`rounded px-1.5 py-0.5 font-medium ${workspaceBadge(parsed.prefix)}`}>
              {parsed.identifier}
            </span>
            <span aria-hidden>→</span>
            <span className="font-mono text-slate-700 dark:text-slate-200">{parsed.hostname}</span>
          </p>
        ) : null}
      </label>

      <p className="mt-3 text-xs font-medium text-slate-500 dark:text-slate-400">
        Preset{" "}
        <span className="font-normal text-slate-700 dark:text-slate-200">
          {preset ? (
            <>
              <span className="font-medium">{preset.name}</span>
              {preset.labels.length > 0 ? ` · ${preset.labels.join(", ")}` : ""}
            </>
          ) : parsed && presets.length > 0 ? (
            // Blocking, not cosmetic: with no preset dropdown there's nothing to override the
            // auto-selection with, so say what to fix.
            <span className="text-red-600 dark:text-red-400">
              no preset claims {parsed.prefix.toUpperCase()} — add it to a preset’s ticket-id
              prefixes in Settings
            </span>
          ) : (
            <span className="text-slate-400 dark:text-slate-500">—</span>
          )}
        </span>
      </p>
    </>
  );
}
