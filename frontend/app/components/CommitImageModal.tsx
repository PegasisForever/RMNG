// Commit a running clone to a new clone-source image (`docker commit` → tagged
// `rmng/template:<name>`; the name is a bare DNS label, the server prepends the
// repo). Opened from the camera action on a managed host row. The name prefills
// to the host id (already a DNS label) and is validated to one (mirrors the
// server's `is_dns_label`). Note: committing bakes the clone's on-disk
// credentials into the image; the server logs a warning line — surfaced here too.
import { useState } from "react";

/** Mirror of the server's `is_dns_label`. */
function isDnsLabel(s: string): boolean {
  return s.length <= 63 && /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(s);
}

export function CommitImageModal({
  hostId,
  busy,
  onClose,
  onCommit,
}: {
  hostId: string;
  busy: boolean;
  onClose: () => void;
  onCommit: (name: string) => void;
}) {
  const [name, setName] = useState(hostId);
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
        className="w-full max-w-md rounded-xl border border-slate-200 bg-white p-5 shadow-xl"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <h3 className="text-sm font-semibold text-slate-900">
          Commit <span className="text-emerald-700">{hostId}</span> to an image
        </h3>
        <p className="mt-1 text-xs text-slate-500">
          Snapshots this clone's filesystem to a new clone-source image (tagged under{" "}
          <code>rmng/template</code>). Other clones can then be created from it.
        </p>

        <label className="mt-4 block text-xs font-medium text-slate-600">
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
            className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none"
          />
          {trimmed && !labelOk ? (
            <p className="mt-1 text-[11px] font-normal text-red-600">
              lowercase letters, digits and hyphens only (no leading/trailing hyphen, ≤63 chars)
            </p>
          ) : (
            <p className="mt-1 text-[11px] font-normal text-slate-400">
              → <code>rmng/template:{trimmed || "…"}</code>
            </p>
          )}
        </label>

        <p className="mt-3 rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-[11px] text-amber-800">
          The commit bakes this clone's on-disk credentials (Claude token, Linear key) into the
          image. Only build images from clones you trust to redistribute.
        </p>

        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100"
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
