// Dropdown of clone-source images, used inside the clone dialog. Takes the image list as
// props (the dashboard owns the `/api/images` fetch), reports the chosen reference up via
// `onChange`, and shows a loading / empty state (empty = no base image yet: the operator must
// build one in the wizard or the Images panel first).
//
// It renders from props alone. Which image a fresh dialog starts on is a session read
// (`lastCloneImage`) resolved by the container, and the age each row shows is measured
// against a `now` the container hands down, so this list draws the same thing for the same
// props on any machine at any hour.
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import { relativeAge, formatBytes } from "~/lib/format";

export function ImagePicker({
  images,
  loading,
  value,
  now,
  onChange,
}: {
  /** The clone-source images (from `listImages`). */
  images: ImageInfo[];
  loading: boolean;
  /** Selected image reference, or null (nothing chosen yet / none available). */
  value: string | null;
  /** Wall-clock milliseconds, for each row's age. */
  now: number;
  onChange: (reference: string) => void;
}) {
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
          {img.base ? " · base" : ""} · {formatBytes(img.sizeBytes)} ·{" "}
          {relativeAge(img.createdAt, now)}
        </option>
      ))}
    </select>
  );
}
