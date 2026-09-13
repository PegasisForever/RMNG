// Rebase dialog's markup: a preset, a rebuild checkbox, and the button bar. It renders
// from props alone — no config fetch, no rebase POST, no operation stream — so every
// state it can be in is a story. RebaseModalContainer owns all three of those and
// hands the results down.
//
// Gen-2 rule: this swaps the clone's system image for the picked preset's image and
// keeps the home dataset, the id, and the clone's own preset bindings (rebase is image
// only, never a preset change). The image builds on miss; the checkbox forces a fresh
// build even when the tag already exists.
import { DropdownSelect } from "~/components/DropdownSelect";
import { ModalShell } from "~/components/ModalShell";
import { OperationProgress } from "~/components/OperationProgress";
import { cloneField, cloneLabel } from "~/components/cloneFieldStyles";
import type { Operation } from "~/lib/types";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

export interface RebaseModalViewProps {
  /** The clone being rebased (shown in the title so the dialog names its target). */
  cloneId: string;
  /** Every configured preset, in config order. */
  presets: PresetRedacted[];
  /** Target preset whose image the clone adopts. */
  preset: string;
  onPresetChange: (name: string) => void;
  /** Force a fresh image build even when the tag exists. */
  rebuild: boolean;
  onRebuildChange: (rebuild: boolean) => void;

  /** The Rebase button may fire. */
  valid: boolean;
  /** A rebase is being started, or one is running. Locks the form and both buttons. */
  busy: boolean;
  /** The failed attempt, in the dialog's own words rather than the page banner's. */
  error: string | null;
  /** The rebase operation this dialog started, once it shows up in the live op list. */
  operation: Operation | null;
  onSubmit: () => void;
  /** The dialog is on screen. The container flips it false when the rebase settles, and the
   *  dialog plays its exit before the unmount rather than blinking out. */
  open?: boolean;
  /** The dialog is finished: unmount it. The exit frames have already played. */
  onClose: () => void;
}

export function RebaseModalView({
  cloneId,
  presets,
  preset,
  onPresetChange,
  rebuild,
  onRebuildChange,
  valid,
  busy,
  error,
  operation,
  onSubmit,
  open = true,
  onClose,
}: RebaseModalViewProps) {
  return (
    // Neither Cancel nor Escape may close over a running rebase, so both are held by the
    // same `busy`: the buttons below, and `dismissible` for the key.
    <ModalShell size="lg" dismissible={!busy} open={open} onExited={onClose}>
      {(close) => (
        <>
          <h3 className="shrink-0 text-sm font-semibold text-slate-900 dark:text-slate-100">
            Rebase {cloneId}
          </h3>
          <p className="mt-1 shrink-0 text-[11px] text-slate-400 dark:text-slate-500">
            Swaps the system image for the picked preset's image. Home, id, and
            the clone's own preset bindings stay — rebase is image only.
          </p>

          <div className="mt-3 space-y-2">
            <label className={`${cloneLabel} font-medium`}>
              Preset
              <DropdownSelect
                rows={
                  presets.length === 0
                    ? [
                        {
                          value: "",
                          label: "No presets configured",
                          disabled: true,
                        },
                      ]
                    : presets.map((p) => ({
                        value: p.name,
                        label: p.name,
                      }))
                }
                value={preset}
                onChange={onPresetChange}
                disabled={busy || presets.length === 0}
                label="Preset"
                className={cloneField}
              />
            </label>
            <label className="flex items-center gap-2 text-xs text-slate-600 dark:text-slate-300">
              <input
                type="checkbox"
                checked={rebuild}
                disabled={busy}
                onChange={(e) => onRebuildChange(e.target.checked)}
                className="size-4 accent-emerald-600"
              />
              Rebuild the preset image even when it already exists
            </label>
          </div>

          {error ? (
            <p className="mt-3 shrink-0 text-[11px] text-red-600 dark:text-red-400">
              {error}
            </p>
          ) : null}

          {operation ? (
            <div className="mt-3 shrink-0">
              <OperationProgress op={operation} />
            </div>
          ) : null}

          <div className="mt-4 flex shrink-0 justify-end gap-2">
            <button
              type="button"
              onClick={() => close()}
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
              {busy ? "Rebasing…" : "Rebase"}
            </button>
          </div>
        </>
      )}
    </ModalShell>
  );
}
