// Dropdown of clone-source images, used inside the clone dialog. Takes the image
// list as props (the dashboard owns the `/api/images` fetch), preselects the
// newest-created image and reports the chosen reference up via `onChange`. Shows a
// loading / empty state (empty = no base image yet: the operator must build one in
// the wizard or the Images panel first).
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
  // Preselect the newest-created image once the list arrives, unless the
  // operator already picked one that still exists.
  useEffect(() => {
    if (images.length === 0) return;
    if (value && images.some((i) => i.reference === value)) return;
    const preferred = images.reduce((newest, img) =>
      new Date(img.createdAt).getTime() > new Date(newest.createdAt).getTime() ? img : newest,
    );
    onChange(preferred.reference);
  }, [images, value, onChange]);

  if (loading && images.length === 0) {
    return <p className="mt-1 text-xs text-slate-400 dark:text-slate-500">Loading images…</p>;
  }
  if (images.length === 0) {
    return (
      <p className="mt-1 rounded-md border border-dashed border-slate-300 p-3 text-center text-[11px] text-slate-400 dark:border-slate-600 dark:text-slate-500">
        No clone-source images yet. Build a base image from the Images panel (or re-run setup)
        first.
      </p>
    );
  }

  return (
    <select
      aria-label="Source image"
      value={value ?? ""}
      onChange={(e) => onChange(e.target.value)}
      className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100"
    >
      {images.map((img) => (
        <option key={img.reference} value={img.reference}>
          {img.reference}
          {img.base ? " · base" : ""} · {formatBytes(img.sizeBytes)} · {relativeAge(img.createdAt)}
        </option>
      ))}
    </select>
  );
}
