// Commit a running clone to a new clone-source image (`docker commit` → tagged
// `<name>:latest`; the name is a bare DNS label used as the full image repository,
// no `rmng/template` prefix). Opened from the camera action on a managed clone row. The name prefills
// to the clone id (already a DNS label) and is validated to one (mirrors the
// server's `is_dns_label`). Note: committing bakes the clone's on-disk
// credentials into the image; the server logs a warning line — surfaced here too.
import { useState } from "react";

/** Mirror of the server's `is_dns_label`. */
function isDnsLabel(s: string): boolean {
  return s.length <= 63 && /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(s);
}

export function CommitImageModal({
  cloneId,
  busy,
  onClose,
  onCommit,
}: {
  cloneId: string;
  busy: boolean;
  onClose: () => void;
  onCommit: (name: string) => void;
}) {
  const [name, setName] = useState(cloneId);
  const trimmed = name.trim();
  const labelOk = isDnsLabel(trimmed);

  function submit() {
    if (!labelOk || busy) return;
    onCommit(trimmed);
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4"
      onClick={onClose}
    >
      <div
        className="w-full max-w-md rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
          Commit <span className="text-emerald-700 dark:text-emerald-400">{cloneId}</span> to an image
        </h3>
        <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
          Snapshots this clone's filesystem to a new clone-source image (tagged{" "}
          <code>{trimmed || "…"}:latest</code>). Other clones can then be created from it.
        </p>

        <label className="mt-4 block text-xs font-medium text-slate-600 dark:text-slate-300">
          Image name
          <input
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submit();
            }}
            placeholder="my-snapshot"
            spellCheck={false}
            className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
          />
          {trimmed && !labelOk ? (
            <p className="mt-1 text-[11px] font-normal text-red-600 dark:text-red-400">
              lowercase letters, digits and hyphens only (no leading/trailing hyphen, ≤63 chars)
            </p>
          ) : (
            <p className="mt-1 text-[11px] font-normal text-slate-400 dark:text-slate-500">
              → <code>{trimmed || "…"}:latest</code>
            </p>
          )}
        </label>

        <p className="mt-3 rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-[11px] text-amber-800 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-400">
          The commit bakes this clone's on-disk credentials (Claude token, Linear key) into the
          image. Only build images from clones you trust to redistribute.
        </p>

        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={submit}
            disabled={!labelOk || busy}
            className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
          >
            {busy ? "Committing…" : "Commit image"}
          </button>
        </div>
      </div>
    </div>
  );
}
