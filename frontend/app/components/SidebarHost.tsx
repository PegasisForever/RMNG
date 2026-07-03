import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { useEffect, useRef, useState } from "react";

import claudeLogo from "../assets/claude.png";
import type { Host, Operation } from "~/lib/types";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import { workspaceBadge } from "~/lib/workspace";

// Text color + label per host state. `working` is sky, `idle` amber (done / awaiting
// the next task / needs you), `offline` rose. The state note carries the color; there
// is no longer a status dot (the unread dot took its place on the title row).
const AGENT_STATUS: Record<NonNullable<Host["monitorState"]>, { text: string; label: string }> = {
  working: { text: "text-sky-600", label: "working" },
  idle: { text: "text-amber-700", label: "idle" },
  offline: { text: "text-rose-500", label: "offline" },
};

function effectiveStatus(host: Host): { text: string; label: string } {
  return AGENT_STATUS[host.monitorState ?? "idle"];
}

/** How this clone's Claude account was chosen + which account is actually in use, for
 *  the sidebar line. `mode` is the selection kind; `email` is the live account (absent
 *  for "none", or when auto/specific hasn't resolved one). Returns null when there's
 *  nothing Claude-related to show (e.g. auto with no accounts configured). */
type ClaudeSel = { mode: "auto" | "group" | "specific" | "none"; group?: string; email?: string };
function claudeSelection(host: Host): ClaudeSel | null {
  const email = host.claudeAccountEmail || undefined;
  const sel = host.claudeSelection;
  if (host.claudeGroup) return { mode: "group", group: host.claudeGroup, email };
  if (sel === "none") return { mode: "none" };
  if (sel === "auto") return email ? { mode: "auto", email } : null;
  if (sel && !sel.startsWith("group:")) return { mode: "specific", email: email ?? sel };
  // Legacy host (no selection recorded): show the account, without a mode badge.
  return email ? { mode: "specific", email } : null;
}

/** Short badge for the non-default selection modes (specific renders as the plain
 *  email, so it — and legacy hosts — get no badge). Group shows the group name. */
function selBadge(sel: ClaudeSel): string | null {
  if (sel.mode === "auto") return "auto";
  if (sel.mode === "none") return "none";
  if (sel.mode === "group") return sel.group ?? "group";
  return null; // specific
}

/** CPU (percent of the clone's cpu allowance) + memory-used strings for the top row,
 *  e.g. `{ cpu: "20%", mem: "3.2GB" }`. Rendered in fixed-width, right-aligned tabular
 *  slots so the figures line up across every row. CPU normalizes `stats.cpuPct` (docker
 *  convention: 100 == one core) by `cloneCpus`; below 1% one decimal is kept so a
 *  near-idle clone doesn't read as dead-zero. When `cloneCpus <= 0` (unlimited clone)
 *  it falls back to a cores figure (`2.4c`). MEM is memory used in GiB, one decimal.
 *  Returns null when there's no usable sample — no stats yet, or a stopped/unmanaged
 *  host with no memory limit. `mem*` are typed bigint by ts-rs but arrive as JSON
 *  numbers, hence the `Number()` coercion. */
function usageParts(
  stats: ContainerStats | undefined,
  cloneCpus: number,
): { cpu: string; mem: string } | null {
  if (!stats) return null;
  const memLimit = Number(stats.memLimit);
  if (memLimit <= 0) return null;
  const GiB = 1024 ** 3;
  const mem = `${(Number(stats.memUsed) / GiB).toFixed(1)}GB`;
  const cpu =
    cloneCpus > 0
      ? (() => {
          const pct = stats.cpuPct / cloneCpus;
          return `${pct < 1 ? pct.toFixed(1) : Math.round(pct)}%`;
        })()
      : `${(stats.cpuPct / 100).toFixed(1)}c`;
  return { cpu, mem };
}

function selTitle(sel: ClaudeSel): string {
  switch (sel.mode) {
    case "group":
      return `Claude group: ${sel.group} (on ${sel.email ?? "?"})`;
    case "auto":
      return `Claude: auto — server picks the best account${sel.email ? ` (on ${sel.email})` : ""}`;
    case "none":
      return "Claude: none — no token installed";
    default:
      return `Claude account: ${sel.email} (fixed)`;
  }
}

export interface SidebarHostProps {
  host: Host;
  /** Live CPU/RAM usage for this host's container, pushed over the `stats` SSE event.
   *  Absent for a stopped/unmanaged host or before the first sample — renders nothing. */
  stats?: ContainerStats;
  /** The fleet's `docker.cloneCpus` CPU allowance (cores per clone), used to normalize
   *  the usage line's CPU figure to a percent of that allowance. `<= 0` means unlimited,
   *  which falls `usageParts` back to a cores figure. */
  cloneCpus: number;
  selected: boolean;
  /** A running operation targeting this host (delete, or a clone finishing its
   *  post-add `wait-swap` step), if any. */
  op?: Operation;
  onSelect: () => void;
  onDelete: () => void;
  /** Commit this managed clone to a new clone-source image. */
  onCommit: () => void;
  /** Change this clone's Claude account/group. */
  onChangeAccount: () => void;
}

function DotsIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor" aria-hidden>
      <circle cx="8" cy="3" r="1.4" />
      <circle cx="8" cy="8" r="1.4" />
      <circle cx="8" cy="13" r="1.4" />
    </svg>
  );
}

/** The per-host overflow menu (⋯) — collapses the commit / change-account / delete
 *  actions. Unmanaged rows (no container) only get Remove. Every trigger/item stops
 *  propagation so opening or invoking an action never selects or drags the row. */
function OverflowMenu({
  hostId,
  managed,
  busy,
  onCommit,
  onChangeAccount,
  onDelete,
}: {
  hostId: string;
  managed: boolean;
  busy: boolean;
  onCommit: () => void;
  onChangeAccount: () => void;
  onDelete: () => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const item = (label: string, onClick: () => void, danger = false) => (
    <button
      type="button"
      role="menuitem"
      onPointerDown={(e) => e.stopPropagation()}
      onClick={(e) => {
        e.stopPropagation();
        setOpen(false);
        onClick();
      }}
      className={`block w-full cursor-pointer px-3 py-1.5 text-left text-xs ${
        danger ? "text-red-600 hover:bg-red-50" : "text-slate-600 hover:bg-slate-100"
      }`}
    >
      {label}
    </button>
  );

  return (
    <div ref={ref} className="relative shrink-0" onClick={(e) => e.stopPropagation()}>
      <button
        type="button"
        aria-label={`actions for ${hostId}`}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={busy}
        onPointerDown={(e) => e.stopPropagation()}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
        className={`cursor-pointer rounded p-1 text-slate-400 hover:bg-slate-200 hover:text-slate-600 disabled:opacity-0 ${
          open ? "bg-slate-200 text-slate-600" : ""
        }`}
      >
        <DotsIcon />
      </button>
      {open ? (
        <div
          role="menu"
          className="absolute right-0 top-full z-20 mt-1 w-40 overflow-hidden rounded-md border border-slate-200 bg-white py-1 shadow-lg"
        >
          {managed ? (
            <>
              {item("Commit to image…", onCommit)}
              {item("Change account…", onChangeAccount)}
              <div className="my-1 h-px bg-slate-100" />
            </>
          ) : null}
          {item(managed ? "Delete" : "Remove", onDelete, true)}
        </div>
      ) : null}
    </div>
  );
}

export function SidebarHost({
  host,
  stats,
  cloneCpus,
  selected,
  op,
  onSelect,
  onDelete,
  onCommit,
  onChangeAccount,
}: SidebarHostProps) {
  const busy = op?.status === "running";
  // Managed clones (backed by a container named after the host id) get the commit /
  // account actions; plain unmanaged rows only get remove.
  const managed = host.managed === true;
  const status = effectiveStatus(host);
  const claudeSel = claudeSelection(host);
  const usage = usageParts(stats, cloneCpus);
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } =
    useSortable({ id: host.id, disabled: busy });

  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    // `position: relative` so the z-index actually takes effect — z-index is ignored
    // on a statically-positioned element, which is why a dragged card otherwise paints
    // *under* the sibling rows that come after it in the DOM. With it positioned, the
    // raised z-index lifts the dragged card above every other row.
    position: "relative",
    zIndex: isDragging ? 50 : undefined,
  };

  return (
    // The whole card is both the drag source (no handle) and the select target — a
    // plain click selects (the sensor's 5px activation distance keeps clicks and drags
    // apart); a drag reorders. The ⋯ menu stops propagation.
    <div
      ref={setNodeRef}
      style={style}
      {...attributes}
      {...listeners}
      aria-pressed={selected}
      onClick={onSelect}
      title={`${host.id} · ${host.host}:${host.port}`}
      className={`group flex touch-none cursor-grab items-start gap-1 border-b border-b-slate-200 border-l-2 border-l-transparent px-1.5 py-1.5 last:border-b-0 active:cursor-grabbing ${
        // Per-side borders (explicit colors so they never collide): a slate-200 bottom
        // divider between rows + a left accent for the selected row. Exactly one
        // background wins (dragging ▸ selected ▸ default); the default is a solid
        // slate-50 (not transparent) so a dragged card fully hides the rows under it.
        // While dragging the card lifts out as a rounded, divider-less floating card.
        isDragging
          ? "rounded-md border-b-transparent bg-white shadow-lg ring-1 ring-slate-300"
          : selected
            ? "border-l-emerald-400 bg-emerald-50"
            : "bg-slate-50 hover:bg-slate-100"
      }`}
    >
      <div className="min-w-0 flex-1">
        {/* Row 1: Claude account (left, truncates) · CPU/MEM (right, fixed-width tabular
            slots so figures align across rows). Hidden while an op is running. */}
        {!busy ? (
          <div className="mb-0.5 flex items-center gap-2 text-[10px]">
            {claudeSel ? (
              <span
                className="flex min-w-0 flex-1 items-center gap-1 text-slate-400"
                title={selTitle(claudeSel)}
              >
                <img src={claudeLogo} alt="" className="h-3 w-3 shrink-0 object-contain" />
                {selBadge(claudeSel) ? (
                  <span className="shrink-0 rounded bg-slate-100 px-1 text-[9px] font-semibold text-slate-500">
                    {selBadge(claudeSel)}
                  </span>
                ) : null}
                {claudeSel.email ? (
                  <span className="truncate">{claudeSel.email}</span>
                ) : claudeSel.mode === "none" ? (
                  <span className="italic text-slate-300">no token</span>
                ) : null}
              </span>
            ) : (
              <span className="min-w-0 flex-1" />
            )}
            {usage ? (
              <span
                className="flex shrink-0 items-baseline gap-1 tabular-nums"
                title="live container CPU (% of clone allowance) · memory used"
              >
                <span className="font-medium text-slate-400">CPU</span>
                <span className="w-8 text-right font-semibold text-slate-700">{usage.cpu}</span>
                <span className="ml-1 font-medium text-slate-400">MEM</span>
                <span className="w-8 text-right font-semibold text-slate-700">{usage.mem}</span>
              </span>
            ) : null}
          </div>
        ) : null}

        {/* Row 2: unread "!" mark + ticket badge inlined with the title, so a wrapped title
            flows back to the left edge on the next line (the badge doesn't indent it).
            While busy, show the op step in place of the title row. */}
        {!busy ? (
          <p className="break-words text-sm font-medium leading-snug text-slate-800">
            {host.unread && !selected ? (
              <span
                className="mr-1 inline-flex h-3.5 w-3.5 items-center justify-center rounded-full bg-red-500 align-middle text-[10px] font-bold leading-none text-white"
                title="stopped working since you last viewed it"
                aria-label="unread: stopped working since last viewed"
              >
                !
              </span>
            ) : null}
            {host.linearWorkspace && host.linearTicket ? (
              <span
                className={`mr-1 inline-block rounded px-1 py-0.5 align-middle text-[10px] font-semibold leading-none ${workspaceBadge(
                  host.linearWorkspace,
                )}`}
              >
                {host.linearTicket}
              </span>
            ) : null}
            {host.displayName ?? host.id}
          </p>
        ) : (
          <div className="flex items-center gap-2">
            <span className="min-w-0 flex-1 break-words text-sm font-medium text-slate-800">
              {host.displayName ?? host.id}
            </span>
            <span className="shrink-0 text-[10px] font-medium text-sky-600">
              {op?.kind === "delete" ? "deleting…" : op?.step}
            </span>
          </div>
        )}

        {/* Row 3: agent state note (or status label fallback), colored by status. */}
        {!busy ? (
          <p
            className={`mt-1 line-clamp-2 text-xs leading-snug ${status.text}`}
            title={host.stateNote || status.label}
          >
            {[host.linearLabel, host.stateNote || status.label].filter(Boolean).join(" · ")}
          </p>
        ) : null}
      </div>

      {/* Commit / change-account / delete collapsed into a ⋯ overflow menu. */}
      <OverflowMenu
        hostId={host.id}
        managed={managed}
        busy={busy}
        onCommit={onCommit}
        onChangeAccount={onChangeAccount}
        onDelete={onDelete}
      />
    </div>
  );
}
