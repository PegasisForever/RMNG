// Settings section listing the clone-source images (`rmng.image=1`). Each row shows
// the reference, a "base" badge for the wizard-pulled template, size, age and the count
// of live clones running on it. Delete is disabled while the image is in use (a live clone's
// `source` points at it). A "+ Pull template" action pulls a registry reference the operator
// names — the pulled image keeps its own `repo:tag` (no local retag). A "Pull latest" action
// re-pulls the configured reference in one click; re-pulling the same `repo:tag` naturally
// moves the tag onto the fresh image, which is the refresh.
//
// The question each of those three asks (which reference? are you sure?) is a `window.prompt`
// or a `window.confirm`, which is a side effect, so it belongs to the container. Every action
// here is a bare callback and every one of them is a click a story can make.
import { RefreshCw, X } from "lucide-react";

import { formatBytes, relativeAge } from "~/lib/format";
import type { ImageInfo } from "~/lib/wire/ImageInfo";

export function ImagesSection({
  images,
  loading,
  pullBusy,
  templateRef,
  now,
  onPullLatest,
  onPullOther,
  onDelete,
}: {
  images: ImageInfo[];
  loading: boolean;
  /** True while a template-pull op is running (disables both pull actions). */
  pullBusy: boolean;
  /** Configured `docker.templateReference`, named in the "Pull latest" button's title. */
  templateRef: string;
  /** Wall-clock milliseconds, for each row's age. Read by the container so this list draws
   *  the same thing for the same props. */
  now: number;
  /** Re-pull the configured template reference. The container confirms first. */
  onPullLatest: () => void;
  /** Pull some other reference. The container asks which one. */
  onPullOther: () => void;
  /** Delete this image. The container confirms first. */
  onDelete: (reference: string) => void;
}) {
  return (
    <div>
      <div className="mb-1 flex items-center justify-between px-1">
        <h2 className="text-[11px] font-semibold uppercase tracking-wide text-slate-400 dark:text-slate-500">
          Images ({images.length})
        </h2>
        <div className="flex items-center gap-1">
          <button
            type="button"
            onClick={onPullLatest}
            disabled={pullBusy}
            title={`Pull the latest template (${templateRef || "configured reference"})`}
            className="flex items-center gap-1 rounded px-1 text-[11px] font-medium text-slate-400 hover:bg-slate-200 hover:text-slate-600 disabled:opacity-40 dark:text-slate-500 dark:hover:bg-slate-700 dark:hover:text-slate-300"
          >
            <RefreshCw className="size-3" />
            Pull latest
          </button>
          <button
            type="button"
            onClick={onPullOther}
            disabled={pullBusy}
            title="Pull the clone template (Ubuntu 26.04) from Docker Hub"
            className="rounded px-1 text-[11px] font-medium text-slate-400 hover:bg-slate-200 hover:text-slate-600 disabled:opacity-40 dark:text-slate-500 dark:hover:bg-slate-700 dark:hover:text-slate-300"
          >
            + Pull template
          </button>
        </div>
      </div>

      {loading && images.length === 0 ? (
        <p className="rounded-lg border border-dashed border-slate-300 bg-white p-3 text-center text-xs text-slate-400 dark:border-slate-600 dark:bg-slate-800 dark:text-slate-500">
          Loading images…
        </p>
      ) : images.length === 0 ? (
        <p className="rounded-lg border border-dashed border-slate-300 bg-white p-4 text-center text-xs text-slate-400 dark:border-slate-600 dark:bg-slate-800 dark:text-slate-500">
          No images yet. Pull the template to start cloning.
        </p>
      ) : (
        <div className="space-y-0.5">
          {images.map((img) => {
            const inUse = img.inUseBy.length;
            return (
              <div
                key={img.reference}
                className="group flex items-center gap-1 rounded-lg border border-transparent px-1.5 py-1.5 hover:bg-slate-100 dark:hover:bg-slate-800"
                title={img.reference}
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5">
                    <span className="min-w-0 flex-1 truncate text-sm font-medium text-slate-800 dark:text-slate-100">
                      {img.reference}
                    </span>
                    {img.base ? (
                      <span className="shrink-0 rounded bg-emerald-100 px-1 py-0.5 text-[9px] font-semibold leading-none text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-400">
                        base
                      </span>
                    ) : null}
                  </div>
                  <p className="mt-0.5 truncate text-[11px] text-slate-400 dark:text-slate-500">
                    {formatBytes(img.sizeBytes)} · {relativeAge(img.createdAt, now)}
                    {inUse > 0 ? ` · ${inUse} clone${inUse === 1 ? "" : "s"}` : ""}
                  </p>
                </div>
                <button
                  type="button"
                  onClick={() => {
                    if (inUse > 0) return;
                    onDelete(img.reference);
                  }}
                  disabled={inUse > 0}
                  aria-label={`delete image ${img.reference}`}
                  title={inUse > 0 ? `in use by ${inUse} clone(s)` : "delete image"}
                  className="rounded px-1 text-slate-400 opacity-0 hover:text-red-600 group-hover:opacity-100 disabled:cursor-not-allowed disabled:opacity-30 disabled:hover:text-slate-400 dark:text-slate-500 dark:hover:text-red-400 dark:disabled:hover:text-slate-500"
                >
                  <X className="size-4" />
                </button>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
