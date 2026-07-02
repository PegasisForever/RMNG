// Radio list of clone-source images, used inside the clone dialog. Takes the
// image list as props (the dashboard owns the `/api/images` fetch), preselects the
// wizard-built base (`base: true`) — or the first image — and reports the chosen
// reference up via `onChange`. Shows a loading / empty state (empty = no base
// image yet: the operator must build one in the wizard or the Images panel first).
import { useEffect } from "react";

import type { ImageInfo } from "~/lib/wire/ImageInfo";
import { relativeAge, formatBytes } from "~/lib/format";

export function ImagePicker({
  images,
  loading,
  value,
  onChange,
}: {
  /** The clone-source images (from `listImages`). */
  images: ImageInfo[];
  loading: boolean;
  /** Selected image reference, or null (nothing chosen yet / none available). */
  value: string | null;
  onChange: (reference: string) => void;
}) {
  // Preselect the base image (or the first) once the list arrives, unless the
  // operator already picked one that still exists.
  useEffect(() => {
    if (images.length === 0) return;
    if (value && images.some((i) => i.reference === value)) return;
    const preferred = images.find((i) => i.base) ?? images[0];
    onChange(preferred.reference);
  }, [images, value, onChange]);

  if (loading && images.length === 0) {
    return <p className="mt-1 text-xs text-slate-400">Loading images…</p>;
  }
  if (images.length === 0) {
    return (
      <p className="mt-1 rounded-md border border-dashed border-slate-300 p-3 text-center text-[11px] text-slate-400">
        No clone-source images yet. Build a base image from the Images panel (or re-run setup)
        first.
      </p>
    );
  }

  return (
    <div className="mt-1 space-y-1.5">
      {images.map((img) => {
        const selected = value === img.reference;
        return (
          <label
            key={img.reference}
            className={`flex cursor-pointer items-center gap-2 rounded-md border px-2.5 py-2 text-xs ${
              selected
                ? "border-emerald-400 bg-emerald-50"
                : "border-slate-200 hover:bg-slate-50"
            }`}
          >
            <input
              type="radio"
              name="clone-image"
              checked={selected}
              onChange={() => onChange(img.reference)}
              className="shrink-0"
            />
            <span className="min-w-0 flex-1 truncate font-medium text-slate-800">
              {img.reference}
            </span>
            {img.base ? (
              <span className="shrink-0 rounded bg-emerald-100 px-1.5 py-0.5 text-[10px] font-semibold text-emerald-700">
                base
              </span>
            ) : null}
            <span className="shrink-0 text-slate-400">
              {formatBytes(img.sizeBytes)} · {relativeAge(img.createdAt)}
            </span>
          </label>
        );
      })}
    </div>
  );
}
