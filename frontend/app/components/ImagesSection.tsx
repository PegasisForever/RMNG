// Sidebar section listing the clone-source images (`rmng.image=1`). Each row shows
// the reference, a "base" badge for the wizard-pulled template, size, age and the count
// of live clones running on it. Delete is confirm-gated and disabled while the
// image is in use (a live clone's `source` points at it). A "+ Pull template" action
// pulls the configured (or overridden) registry reference and tags it locally
// (prompts for the reference, then a DNS-label local name).
import { formatBytes, relativeAge } from "~/lib/format";
import type { ImageInfo } from "~/lib/wire/ImageInfo";

/** Mirror of the server's `is_dns_label` (local image name → `rmng/template:<name>`). */
function isDnsLabel(s: string): boolean {
  return s.length <= 63 && /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(s);
}

export function ImagesSection({
  images,
  loading,
  pullBusy,
  templateRef,
  onPull,
  onDelete,
}: {
  images: ImageInfo[];
  loading: boolean;
  /** True while a template-pull op is running (disables the pull action). */
  pullBusy: boolean;
  /** Configured `docker.templateReference`, prefilled into the reference prompt. */
  templateRef: string;
  onPull: (name: string, reference: string) => void;
  onDelete: (reference: string) => void;
}) {
  function pull() {
    if (pullBusy) return;
    const rawRef = window.prompt("Template reference to pull (Docker Hub repo:tag)", templateRef);
    if (rawRef == null) return;
    const reference = rawRef.trim();
    if (!reference) {
      alert("Enter a template reference.");
      return;
    }
    const rawName = window.prompt("Name for the local image (→ rmng/template:<name>)", "base");
    if (rawName == null) return;
    const name = rawName.trim();
    if (!isDnsLabel(name)) {
      alert("Invalid name: lowercase letters, digits and hyphens only (no leading/trailing hyphen, ≤63 chars).");
      return;
    }
    onPull(name, reference);
  }

  return (
    <div>
      <div className="mb-1 flex items-center justify-between px-1">
        <h2 className="text-[11px] font-semibold uppercase tracking-wide text-slate-400">
          Images ({images.length})
        </h2>
        <button
          type="button"
          onClick={pull}
          disabled={pullBusy}
          title="Pull the clone template (Ubuntu 26.04) from Docker Hub"
          className="rounded px-1 text-[11px] font-medium text-slate-400 hover:bg-slate-200 hover:text-slate-600 disabled:opacity-40"
        >
          + Pull template
        </button>
      </div>

      {loading && images.length === 0 ? (
        <p className="rounded-lg border border-dashed border-slate-300 bg-white p-3 text-center text-xs text-slate-400">
          Loading images…
        </p>
      ) : images.length === 0 ? (
        <p className="rounded-lg border border-dashed border-slate-300 bg-white p-4 text-center text-xs text-slate-400">
          No images yet. Pull the template to start cloning.
        </p>
      ) : (
        <div className="space-y-0.5">
          {images.map((img) => {
            const inUse = img.inUseBy.length;
            return (
              <div
                key={img.reference}
                className="group flex items-center gap-1 rounded-lg border border-transparent px-1.5 py-1.5 hover:bg-slate-100"
                title={img.reference}
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5">
                    <span className="min-w-0 flex-1 truncate text-sm font-medium text-slate-800">
                      {img.reference}
                    </span>
                    {img.base ? (
                      <span className="shrink-0 rounded bg-emerald-100 px-1 py-0.5 text-[9px] font-semibold leading-none text-emerald-700">
                        base
                      </span>
                    ) : null}
                  </div>
                  <p className="mt-0.5 truncate text-[11px] text-slate-400">
                    {formatBytes(img.sizeBytes)} · {relativeAge(img.createdAt)}
                    {inUse > 0 ? ` · ${inUse} clone${inUse === 1 ? "" : "s"}` : ""}
                  </p>
                </div>
                <button
                  type="button"
                  onClick={() => {
                    if (inUse > 0) return;
                    if (
                      confirm(
                        `Delete image ${img.reference}?\n\nThis removes the image from the Docker daemon.`,
                      )
                    )
                      onDelete(img.reference);
                  }}
                  disabled={inUse > 0}
                  aria-label={`delete image ${img.reference}`}
                  title={inUse > 0 ? `in use by ${inUse} clone(s)` : "delete image"}
                  className="rounded px-1 text-base leading-none text-slate-400 opacity-0 hover:text-red-600 group-hover:opacity-100 disabled:cursor-not-allowed disabled:opacity-30 disabled:hover:text-slate-400"
                >
                  ×
                </button>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
