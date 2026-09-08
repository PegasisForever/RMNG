// Template-create dialog's markup: a title, a preset, and the button bar. It renders
// from props alone — no config fetch, no clone POST, no operation stream — so every
// state it can be in is a story. TemplateModalContainer owns all three of those and
// hands the results down.
//
// Gen-2 rule: this dialog creates from a template image onto a FRESH EMPTY home dataset.
// Two fields only: the clone title and the preset. Always headed, always the preset's
// default Claude and Codex accounts. The new home starts empty; the agent pulls the
// repo itself. (Forking a live clone lives in the New clone dialog.)
import { OperationProgress } from "~/components/OperationProgress";
import { cloneField, cloneLabel } from "~/components/cloneFieldStyles";
import type { Operation } from "~/lib/types";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
import { useModalEscape } from "~/lib/useModalEscape";

export interface TemplateModalViewProps {
  /** Container title for the new clone (plain mode: doubles as the hostname slug base). */
  title: string;
  onTitleChange: (title: string) => void;
  /** Every configured preset, in config order. */
  presets: PresetRedacted[];
  /** Hand-picked preset driving env + accounts. */
  preset: string;
  onPresetChange: (name: string) => void;
  /** Resolved base image for the picked preset, shown as read-only text. */
  presetImage: string | null;

  /** The Clone button may fire. */
  valid: boolean;
  /** A clone is being started, or one is running. Locks the form and both buttons. */
  busy: boolean;
  /** The failed attempt, in the dialog's own words rather than the page banner's. */
  error: string | null;
  /** The clone operation this dialog started, once it shows up in the live op list. */
  operation: Operation | null;
  onSubmit: () => void;
  onClose: () => void;
}

export function TemplateModalView({
  title,
  onTitleChange,
  presets,
  preset,
  onPresetChange,
  presetImage,
  valid,
  busy,
  error,
  operation,
  onSubmit,
  onClose,
}: TemplateModalViewProps) {
  // Escape closes regardless of focus — a document-level listener since the backdrop click no
  // longer does (see below). Guarded the same as the backdrop was: no closing out from under a
  // running clone operation. While `busy` the dialog still holds its slot in the Escape stack,
  // so the keypress is swallowed rather than falling through to whatever is mounted beneath.
  useModalEscape(onClose, !busy);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4">
      {/* Backdrop is inert — clicking it must not close the dialog (nor could it while
          `busy`); only Cancel/Escape do, both guarded against closing over a running
          clone operation. */}
      <div className="flex max-h-[90vh] w-full max-w-lg flex-col rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800">
        <h3 className="shrink-0 text-sm font-semibold text-slate-900 dark:text-slate-100">
          New clone from template
        </h3>
        <p className="mt-1 shrink-0 text-[11px] text-slate-400 dark:text-slate-500">
          Starts with an empty home on its own dataset — the agent pulls the repo itself.
          To copy a live clone instead, fork it from the New clone dialog.
        </p>

        <div className="mt-3 space-y-2">
          <label className={`${cloneLabel} font-medium`}>
            Clone title
            <input
              value={title}
              disabled={busy}
              onChange={(e) => onTitleChange(e.target.value)}
              placeholder="encoder-scratch"
              className={cloneField}
              onKeyDown={(e) => {
                if (e.key === "Enter") onSubmit();
              }}
            />
          </label>
          <label className={`${cloneLabel} font-medium`}>
            Preset
            <select
              value={preset}
              disabled={busy || presets.length === 0}
              onChange={(e) => onPresetChange(e.target.value)}
              className={cloneField}
            >
              {presets.length === 0 ? (
                <option value="" disabled>
                  No presets configured
                </option>
              ) : (
                presets.map((p) => (
                  <option key={p.name} value={p.name}>
                    {p.name}
                  </option>
                ))
              )}
            </select>
            {presetImage ? (
              <p className="mt-1 font-mono text-[11px] text-slate-400 dark:text-slate-500">
                base: {presetImage}
              </p>
            ) : null}
          </label>
        </div>

        {error ? (
          <p className="mt-3 shrink-0 text-[11px] text-red-600 dark:text-red-400">{error}</p>
        ) : null}

        {operation ? (
          <div className="mt-3 shrink-0">
            <OperationProgress op={operation} />
          </div>
        ) : null}

        <div className="mt-4 flex shrink-0 justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            disabled={busy}
            className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100 disabled:opacity-40 dark:text-slate-300 dark:hover:bg-slate-800"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={onSubmit}
            disabled={!valid || busy}
            className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
          >
            {busy ? "Cloning…" : "Clone"}
          </button>
        </div>
      </div>
    </div>
  );
}
