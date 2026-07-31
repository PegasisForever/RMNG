import type { DraggableAttributes, DraggableSyntheticListeners } from "@dnd-kit/core";
import { ArrowRight, ChevronDown, ChevronRight, EllipsisVertical, Terminal } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import chatgptLogo from "../assets/chatgpt.svg";
import claudeLogo from "../assets/claude.svg";
import { copyText } from "~/lib/clipboard";
import { formatTokenCount } from "~/lib/format";
import { buildSshCommand } from "~/lib/ssh";
import type { Clone, Operation } from "~/lib/types";
import type { CloneTokens } from "~/lib/wire/CloneTokens";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { ForwardRuntime } from "~/lib/wire/ForwardRuntime";
import type { ForwardState } from "~/lib/wire/ForwardState";
import type { PortForward } from "~/lib/wire/PortForward";
import { workspaceBadge } from "~/lib/workspace";

// The control server owns this compact lifecycle indicator: blue = recent token activity,
// gray = Docker-running but inactive, purple = Docker stopped/gone. An unread working→not-working
// transition replaces the dot with the red `!` badge below.
//
// Each dot glows in its own color: two box-shadow stops, a tight bright one and a wide dim
// one, which is what reads as light rather than as a ring. The colors are spelled out
// because a shadow cannot inherit the `bg-*` it belongs to. Idle glows far more faintly than
// the rest — a dot that means "nothing is happening" should not be the brightest thing on
// the card.
const STATUS_DOT: Record<NonNullable<Clone["monitorState"]>, { dot: string; label: string }> = {
  working: {
    dot: "bg-blue-500 shadow-[0_0_4px_rgb(59_130_246_/_0.7),0_0_10px_rgb(59_130_246_/_0.45)]",
    label: "working",
  },
  idle: {
    dot: "bg-slate-400 shadow-[0_0_4px_rgb(148_163_184_/_0.45)] dark:bg-slate-500 dark:shadow-[0_0_4px_rgb(100_116_139_/_0.55)]",
    label: "not working",
  },
  offline: {
    dot: "bg-purple-500 shadow-[0_0_4px_rgb(168_85_247_/_0.7),0_0_10px_rgb(168_85_247_/_0.45)]",
    label: "offline",
  },
};

type Metric = { label: string; value: string; title: string };

/** CPU (percentage of total host capacity) + memory-used strings, e.g.
 *  `{ cpu: "20%", mem: "3.2GB" }`. CPU rides the Claude line and MEM the Codex line;
 *  each renders in a fixed-width, right-aligned tabular slot so the two figures stack
 *  and line up across every row. Below 1% one decimal is kept so a near-idle clone does
 *  not read as dead-zero. Memory includes swap and tmpfs/shared memory while excluding
 *  reclaimable page cache. Returns null when there is no usable sample. `mem*` are typed
 *  bigint by ts-rs but arrive as JSON numbers, hence the `Number()` coercion. */
export function formatCloneUsage(
  stats: ContainerStats | undefined,
): { cpu: string; mem: string } | null {
  if (!stats) return null;
  const GiB = 1024 ** 3;
  const mem = `${(Number(stats.memUsed) / GiB).toFixed(1)}GB`;
  const pct = stats.cpuPct;
  const cpu = `${pct < 1 ? pct.toFixed(1) : Math.round(pct)}%`;
  return { cpu, mem };
}

/** A usage metric (↑/↓ tokens, CPU, or MEM): a label + a fixed-width tabular value.
 *
 *  Each metric occupies one column across the card's two rows, so BOTH parts are fixed-width —
 *  that is the only thing keeping a column's two entries in vertical line. The value width does
 *  it for the digits. `labelWidth` does it for the label, and is needed because the account tag
 *  to the left is `flex-1`: any difference in a slot's total width is absorbed there and shows
 *  up as the whole slot sliding sideways.
 *
 *  With plain text labels that never bites — `CPU` and `MEM` are both three uppercase glyphs, so
 *  they measure the same. The arrows do NOT: `↑` and `↓` come out of a fallback font with
 *  per-glyph advance widths, which shifted the ↓ row a few px against the ↑ row.
 *
 *  `metric` is `undefined` for a clone with no sample (stopped, archived, unmanaged, or not yet
 *  scanned); the slot still renders, empty, so the row above never sits at a different width
 *  from the row below. */
function MetricSlot({ metric, labelWidth = "w-6" }: { metric?: Metric; labelWidth?: string }) {
  return (
    <span
      className="flex shrink-0 items-baseline gap-1 tabular-nums"
      title={metric?.title}
      aria-hidden={metric ? undefined : true}
    >
      <span
        className={`${labelWidth} shrink-0 text-right font-medium text-slate-400 dark:text-slate-500`}
      >
        {metric?.label ?? ""}
      </span>
      <span className="w-8 text-right font-semibold text-slate-700 dark:text-slate-200">
        {metric?.value ?? ""}
      </span>
    </span>
  );
}

/** The clone's account binding for one provider: `[logo] [group badge] [account in use]`.
 *
 *  `email` is the account actually installed; `selection` is the operator's intent verbatim
 *  (`auto` / `none` / `group:<pool>` / a pinned email). Both are shown because they answer
 *  different questions: an `auto` clone landed on some concrete account, and only the selection
 *  says whether it may be hot-swapped out from under you. A clone with neither is tokenless.
 *
 *  Only the group badge carries a background — it is the *rule*, a fixed short token that reads
 *  as a chip. The account is the *result*, variable-length and the thing you actually scan for,
 *  so it stays plain text and takes the remaining width, truncating before the metric slot.
 *
 *  `fable` lights a second chip when the clone's most recent response came from the Fable model
 *  within the last few minutes. Server-derived (see `wire::CloneTokens.fableActive`) and it
 *  decays on its own, so this only ever renders what is currently true. */
function AccountTag({
  logo,
  provider,
  email,
  selection,
  fable = false,
}: {
  logo: string;
  provider: string;
  email?: string;
  selection?: string;
  fable?: boolean;
}) {
  const pool = selection?.startsWith("group:") ? selection.slice("group:".length) : undefined;
  // A pinned selection has no badge: the selection *is* the email already beside it, so a chip
  // would just repeat it. Absent both, the clone is tokenless and only the placeholder shows.
  const badge = pool ?? (selection === "auto" ? "auto" : undefined);
  const mode = pool ? `pool ${pool}` : selection === "auto" ? "auto" : undefined;
  const title = email
    ? `${provider}: ${email}${mode ? ` (${mode})` : " (pinned)"}`
    : selection === "none"
      ? `${provider}: no account (deliberately tokenless)`
      : `${provider}: no account assigned`;
  return (
    <span
      className="flex min-w-0 flex-1 items-center gap-1 text-slate-400 dark:text-slate-500"
      title={title}
    >
      {/* The ChatGPT mark ships with no `fill`, so it paints black and vanishes on a dark
          background — invert it there. `claude.svg` carries its own fill and must NOT be
          inverted, hence keying off the asset rather than putting the class on both tags. */}
      <img
        src={logo}
        alt=""
        className={`h-3 w-3 shrink-0 opacity-70 ${logo === chatgptLogo ? "dark:invert" : ""}`}
      />
      {badge ? (
        <span className="shrink-0 rounded bg-slate-200 px-1 text-[9px] font-semibold text-slate-600 dark:bg-slate-700 dark:text-slate-300">
          {badge}
        </span>
      ) : null}
      {email ? (
        <span className="min-w-0 truncate text-[10px] font-medium text-slate-600 dark:text-slate-300">
          {email}
        </span>
      ) : (
        <span className="truncate italic text-slate-300 dark:text-slate-600">
          {selection === "none" ? "no token" : "unassigned"}
        </span>
      )}
      {/* Placed after the email so it stays visible when a long address truncates: it is
          transient news about right now, whereas the address is stable and re-readable on
          hover. `shrink-0` keeps the truncation pressure on the email rather than the chip. */}
      {fable ? (
        <span
          title="recently served by the Fable model"
          className="shrink-0 rounded bg-amber-100 px-1 text-[9px] font-semibold text-amber-700 dark:bg-amber-900 dark:text-amber-300"
        >
          fable
        </span>
      ) : null}
    </span>
  );
}

// Status dot per forward state (+ a muted "disabled" for rules toggled off), shown in
// the compact per-clone forwards chips.
const FORWARD_DOT: Record<ForwardState | "disabled", string> = {
  listening: "bg-emerald-500",
  error: "bg-red-500",
  offline: "bg-slate-400 dark:bg-slate-500",
  disabled: "bg-slate-300 dark:bg-slate-600",
};

/** A compact wrapping row of this clone's port forwards — one `remote→local` chip per
 *  rule with a status-colored dot, live state merged from the `forwards` SSE event by
 *  rule id. A disabled rule renders muted; hover shows the full mapping + state/error. */
function ForwardChips({ forwards, runtime }: { forwards: PortForward[]; runtime: ForwardRuntime[] }) {
  const rtById = new Map(runtime.map((r) => [r.id, r]));
  return (
    <div className="mt-1 flex flex-wrap gap-1">
      {forwards.map((f) => {
        const rt = rtById.get(f.id);
        const state: ForwardState | "disabled" = !f.enabled ? "disabled" : rt?.state ?? "offline";
        const conns = rt && rt.activeConns > 0 ? ` · ${rt.activeConns} conn` : "";
        const err = rt?.error ? ` · ${rt.error}` : "";
        return (
          <span
            key={f.id}
            title={`${f.remotePort} → 127.0.0.1:${f.localPort} · ${state}${conns}${err}`}
            className={`inline-flex items-center gap-1 rounded bg-slate-100 px-1 py-0.5 font-mono text-[9px] font-medium text-slate-500 dark:bg-slate-800 dark:text-slate-400 ${
              f.enabled ? "" : "opacity-60"
            }`}
          >
            <span className={`size-1.5 shrink-0 rounded-full ${FORWARD_DOT[state]}`} />
            {f.remotePort}
            <ArrowRight className="size-2.5 shrink-0 text-slate-500 dark:text-slate-400" />
            {f.localPort}
          </span>
        );
      })}
    </div>
  );
}

export interface SidebarCloneProps {
  clone: Clone;
  /** Live CPU/RAM usage for this clone's container, pushed over the `stats` SSE event.
   *  Absent for a stopped/unmanaged clone or before the first sample — renders nothing. */
  stats?: ContainerStats;
  /** All-time token totals for this clone, summed across Claude and Codex, read from the
   *  agents' own session logs. Part of `ControlState` (persisted) rather than a volatile
   *  bus, so it arrives with the ordinary state snapshot. Absent until the clone's first
   *  scan produces a number. */
  tokens?: CloneTokens;
  selected: boolean;
  /** A running operation targeting this clone (delete, or a clone finishing its
   *  post-add `wait-swap` step), if any. */
  op?: Operation;
  onSelect: () => void;
  onDelete: () => void;
  /** Commit this managed clone to a new clone-source image. */
  onCommit: () => void;
  /** Change this clone's account-group binding. */
  onChangeAccount: () => void;
  /** Open the port-forward editor for this clone. */
  onPortForward: () => void;
  /** Gracefully stop a managed clone while retaining it. */
  onArchive: () => void;
  /** Restart a retained managed clone. */
  onUnarchive: () => void;
  /** Live runtime status for this clone's forwards (from the `forwards` SSE event),
   *  merged into the compact forwards chips by rule id. */
  forwardRuntime?: ForwardRuntime[];
  /** `ssh.publicHost` (config) — the `-J` jump target for the copied command. Empty ⇒
   *  falls back to `window.location.hostname` (this page's own address). */
  sshPublicHost: string;
  /** `listen.bastion` — the bastion `sshd` port the copied command jumps through. */
  bastionPort: number;
  /** True when this row is a sub clone: it renders indented under its parent and is not
   *  drag-reorderable (nesting is a cosmetic one-level grouping). */
  isChild?: boolean;
  /** Number of sub clones under this (top-level) clone. `> 0` shows the expand/collapse control
   *  at the bottom of the card. */
  childCount?: number;
  /** Whether this clone's sub clones are currently expanded. */
  expanded?: boolean;
  /** Toggle this clone's sub-clone expansion. */
  onToggleExpand?: () => void;
  /** dnd-kit drag activator props from the enclosing sortable group (see `SortableCloneGroup`).
   *  Present only on a draggable top-level row; spread onto the card so grabbing it drags the
   *  whole group (parent + its expanded sub clones). Absent ⇒ the row is static (children,
   *  archived rows, Storybook). */
  dragAttributes?: DraggableAttributes;
  dragListeners?: DraggableSyntheticListeners;
  /** True while this row's group is being dragged (drives the lifted-card styling). */
  dragging?: boolean;
}

/** A single overflow-menu item that copies `command` to the clipboard and shows a
 *  brief "Copied!" label before asking the menu to close. Kept separate from the
 *  plain-text `item()` helper because it needs its own transient state + delayed
 *  close (the other items close immediately on click). */
function CopySshMenuItem({ command, onDone }: { command: string; onDone: () => void }) {
  // `null` = idle, `true` = copied, `false` = copy failed (both clipboard paths refused,
  // e.g. execCommand blocked). Only claim "Copied!" on a genuine success so the label
  // never lies about what reached the clipboard.
  const [result, setResult] = useState<boolean | null>(null);
  return (
    <button
      type="button"
      role="menuitem"
      onPointerDown={(e) => e.stopPropagation()}
      onClick={async (e) => {
        e.stopPropagation();
        const ok = await copyText(command);
        setResult(ok);
        // On failure keep the menu open a beat longer so the user can select the
        // command text (shown in the title) and copy it by hand.
        setTimeout(onDone, ok ? 900 : 1600);
      }}
      title={command}
      className="flex w-full items-center gap-1.5 px-3 py-1.5 text-left text-xs text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-700"
    >
      <Terminal className="size-4 shrink-0" />
      {result === true ? "Copied!" : result === false ? "Copy failed — copy manually" : "Copy SSH command"}
    </button>
  );
}

/** The per-clone overflow menu (⋮) — collapses the commit / change-account / delete
 *  actions. Unmanaged rows (no container) only get Remove. Every trigger/item stops
 *  propagation so opening or invoking an action never selects or drags the row. */
function OverflowMenu({
  cloneId,
  managed,
  archived,
  busy,
  onCommit,
  onChangeAccount,
  onPortForward,
  onArchive,
  onUnarchive,
  onDelete,
  sshCommand,
}: {
  cloneId: string;
  managed: boolean;
  archived: boolean;
  busy: boolean;
  onCommit: () => void;
  onChangeAccount: () => void;
  onPortForward: () => void;
  onArchive: () => void;
  onUnarchive: () => void;
  onDelete: () => void;
  /** The ready-to-paste `ssh -J …` one-liner for this clone. Undefined for unmanaged
   *  rows (no real container/sshd to jump to), which hides the menu item. */
  sshCommand?: string;
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
        danger
          ? "text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-950/40"
          : "text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-700"
      }`}
    >
      {label}
    </button>
  );

  return (
    <div ref={ref} className="relative shrink-0" onClick={(e) => e.stopPropagation()}>
      <button
        type="button"
        aria-label={`actions for ${cloneId}`}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={busy}
        onPointerDown={(e) => e.stopPropagation()}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
        className={`cursor-pointer rounded p-1 text-slate-400 hover:bg-slate-200 hover:text-slate-600 disabled:opacity-0 dark:text-slate-500 dark:hover:bg-slate-700 dark:hover:text-slate-300 ${
          open ? "bg-slate-200 text-slate-600 dark:bg-slate-700 dark:text-slate-300" : ""
        }`}
      >
        <EllipsisVertical className="size-4" />
      </button>
      {open ? (
        <div
          role="menu"
          className="absolute right-0 top-full z-20 mt-1 w-40 overflow-hidden rounded-md border border-slate-200 bg-white py-1 shadow-lg dark:border-slate-700 dark:bg-slate-800"
        >
          {managed && !archived ? (
            <>
              {item("Commit to image…", onCommit)}
              {item("Change account…", onChangeAccount)}
              {item("Port forward…", onPortForward)}
              {sshCommand ? <CopySshMenuItem command={sshCommand} onDone={() => setOpen(false)} /> : null}
              {item("Archive", onArchive)}
              <div className="my-1 h-px bg-slate-100 dark:bg-slate-700" />
            </>
          ) : null}
          {managed && archived ? (
            <>
              {item("Unarchive", onUnarchive)}
              <div className="my-1 h-px bg-slate-100 dark:bg-slate-700" />
            </>
          ) : null}
          {item(managed ? "Delete" : "Remove", onDelete, true)}
        </div>
      ) : null}
    </div>
  );
}

export function SidebarClone({
  clone,
  stats,
  tokens,
  selected,
  op,
  onSelect,
  onDelete,
  onCommit,
  onChangeAccount,
  onPortForward,
  onArchive,
  onUnarchive,
  forwardRuntime,
  sshPublicHost,
  bastionPort,
  isChild = false,
  childCount = 0,
  expanded = false,
  onToggleExpand,
  dragAttributes,
  dragListeners,
  dragging = false,
}: SidebarCloneProps) {
  const busy = op?.status === "running";
  // Managed clones (backed by a container named after the clone id) get the commit /
  // account actions; plain unmanaged rows only get remove.
  const managed = clone.managed === true;
  // Archived clones retain their container but deliberately hide runtime actions until they
  // are restored; unmanaged rows have no container-backed SSH endpoint either.
  const sshCommand = managed && !clone.archived
    ? buildSshCommand(sshPublicHost || window.location.hostname, bastionPort, clone.id)
    : undefined;
  const status = clone.archived ? undefined : STATUS_DOT[clone.monitorState ?? "idle"];
  const usage = clone.archived ? null : formatCloneUsage(stats);
  const cpuMetric = usage
    ? { label: "CPU", value: usage.cpu, title: "live container CPU (% of total host capacity)" }
    : undefined;
  const memMetric = usage
    ? {
        label: "MEM",
        value: usage.mem,
        title: "RAM + swap; includes tmpfs/shared memory and excludes reclaimable file cache",
      }
    : undefined;
  // All-time token totals, summed across both providers. Unlike CPU/MEM these survive the
  // clone going quiet, so they render for an archived clone too — the work already happened.
  // A clone with no scan yet has no slot at all rather than a misleading "0".
  const inMetric = tokens
    ? {
        label: "↑",
        value: formatTokenCount(tokens.inputTokens),
        title:
          "all-time input tokens (Claude + Codex); newly processed only — cache reads excluded",
      }
    : undefined;
  const outMetric = tokens
    ? {
        label: "↓",
        value: formatTokenCount(tokens.outputTokens),
        title: "all-time output tokens (Claude + Codex), including Codex reasoning tokens",
      }
    : undefined;
  // Managed clones always show the binding line: an unassigned account is itself worth seeing.
  const showBindingLine = managed || !!cpuMetric;
  // Drag is owned by the enclosing SortableCloneGroup; a row is draggable only when it received
  // drag listeners (top-level active rows). Children/archived rows get none and stay static.
  const draggable = !!dragListeners;

  return (
    // The whole card is both the drag source (no handle) and the select target — a
    // plain click selects (the sensor's 5px activation distance keeps clicks and drags
    // apart); a drag reorders. The ⋯ menu and the expand control stop propagation.
    <div
      {...dragAttributes}
      {...dragListeners}
      aria-pressed={selected}
      onClick={onSelect}
      title={clone.id}
      className={`group flex touch-none items-start gap-1 border-l-2 border-l-transparent pr-1.5 pb-2.5 pt-1.5 ${
        // Sub clone rows are indented under their parent; top-level rows keep the normal gutter.
        isChild ? "pl-6" : "pl-1.5"
      } ${draggable ? "cursor-grab active:cursor-grabbing" : "cursor-pointer"} ${
        // The only border is the left accent for the selected row. A row carries no bottom
        // divider: one card holds one row, so a divider there would just double the card's
        // own outline and read as a heavier bottom edge.
        //
        // Exactly one background wins (dragging ▸ selected ▸ default). The default is a
        // solid white (not transparent) so a dragged card fully hides the rows it passes
        // over, and white is what leaves the selected tint and the hover as the only colour
        // in a column.
        dragging
          ? "rounded-md bg-white shadow-lg ring-1 ring-slate-300 dark:bg-slate-800 dark:ring-slate-600"
          : selected
            ? "border-l-emerald-400 bg-emerald-50 dark:bg-emerald-950"
            : "bg-white hover:bg-slate-50 dark:bg-slate-900 dark:hover:bg-slate-800"
      }`}
    >
      <div className="min-w-0 flex-1">
        {/* Two stacked rows above the title — Claude + CPU, then Codex + MEM — with the ⋮ menu
            spanning both. Each provider gets a full line, so an email that used to truncate at a
            few characters is now readable, and CPU/MEM sit in the same fixed slot on each row so
            the digits line up vertically. While busy, the op step replaces both rows.

            `items-center` is what makes the ⋮ button span the pair: it is a flex sibling of the
            two-row column, so it centres against the column's full height rather than sitting on
            the Claude row. */}
        <div className="mb-0 flex items-center gap-1">
          {busy ? (
            <div className="flex min-w-0 flex-1 items-center gap-2">
              <span className="min-w-0 flex-1 break-words text-sm font-medium text-slate-800 dark:text-slate-100">
                {clone.displayName ?? clone.id}
              </span>
              <span className="shrink-0 text-[10px] font-medium text-sky-600 dark:text-sky-400">
                {op?.kind === "delete" ? "deleting…" : op?.step}
              </span>
            </div>
          ) : showBindingLine ? (
            // Both rows always render for a managed clone, placeholder text included, so card
            // height is identical across the list and the sidebar doesn't jump as clones change
            // state. `min-w-0` on the column is what lets the tags inside actually truncate.
            <div className="flex min-w-0 flex-1 flex-col gap-0.5 text-[10px]">
              {/* Each row: account … tokens … CPU-or-MEM. The token figures are a whole-clone
                  total (both providers summed), so they deliberately do NOT line up with the
                  provider named at the left of their own row — ↑ simply rides the first line
                  and ↓ the second, because those are the two lines that exist. */}
              <div className="flex min-w-0 items-center gap-2">
                <AccountTag
                  logo={claudeLogo}
                  provider="Claude"
                  email={clone.claudeAccountEmail}
                  selection={clone.claudeSelection}
                  fable={tokens?.fableActive}
                />
                {/* The arrow labels are one glyph, so they get their own narrow width rather
                    than the three-character one CPU/MEM need. */}
                <MetricSlot metric={inMetric} labelWidth="w-2" />
                <MetricSlot metric={cpuMetric} />
              </div>
              <div className="flex min-w-0 items-center gap-2">
                <AccountTag
                  logo={chatgptLogo}
                  provider="Codex"
                  email={clone.codexAccountEmail}
                  selection={clone.codexSelection}
                />
                <MetricSlot metric={outMetric} labelWidth="w-2" />
                <MetricSlot metric={memMetric} />
              </div>
            </div>
          ) : (
            <div className="min-w-0 flex-1" />
          )}
          <OverflowMenu
            cloneId={clone.id}
            managed={managed}
            archived={clone.archived ?? false}
            busy={busy}
            onCommit={onCommit}
            onChangeAccount={onChangeAccount}
            onPortForward={onPortForward}
            onArchive={onArchive}
            onUnarchive={onUnarchive}
            onDelete={onDelete}
            sshCommand={sshCommand}
          />
        </div>

        {/* Title: unread "!" mark + ticket badge inlined with the title, so a wrapped title
            flows back to the left edge on the next line (the badge doesn't indent it).
            Hidden while busy — the op step shows in the top block instead.

            The top margin lives here rather than on the block above, because that block also
            holds the busy-state title — a bottom margin there would leave dead space under it. */}
        {!busy ? (
          <p className="mt-1.5 break-words text-sm font-medium leading-snug text-slate-800 dark:text-slate-100">
            {clone.unread && !selected ? (
              <span
                className="mr-1 inline-flex size-3 items-center justify-center rounded-full bg-red-500 align-middle text-[10px] font-bold leading-none text-white shadow-[0_0_4px_rgb(239_68_68_/_0.7),0_0_10px_rgb(239_68_68_/_0.45)]"
                title="was working and is no longer working"
                aria-label="unread: working transitioned to not working"
              >
                !
              </span>
            ) : status ? (
              <span
                className={`mr-1 inline-block size-3 rounded-full align-middle ${status.dot}`}
                title={status.label}
                aria-label={status.label}
              />
            ) : null}
            {clone.linearWorkspace && clone.linearTicket ? (
              <span
                className={`mr-1 inline-block rounded px-1 py-0.5 align-middle text-[10px] font-semibold leading-none ${workspaceBadge(
                  clone.linearWorkspace,
                )}`}
              >
                {clone.linearTicket}
              </span>
            ) : null}
            {clone.headless ? (
              <Terminal
                className="mr-1 inline-block size-3.5 align-middle text-slate-500 dark:text-slate-400"
                aria-label="headless clone (tmux view)"
              />
            ) : null}
            {clone.displayName ?? clone.id}
          </p>
        ) : null}

        {/* Compact list of this clone's port forwards (remote→local, live status dot). */}
        {!busy && clone.forwards && clone.forwards.length > 0 ? (
          <ForwardChips forwards={clone.forwards} runtime={forwardRuntime ?? []} />
        ) : null}

        {/* Expand/collapse this clone's sub clones — pinned to the bottom of the card. Stops
            propagation so it neither selects the row nor starts a drag. */}
        {!busy && childCount > 0 ? (
          <button
            type="button"
            aria-expanded={expanded}
            title={`${expanded ? "hide" : "show"} ${childCount} sub clone${childCount === 1 ? "" : "s"}`}
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              onToggleExpand?.();
            }}
            className="mt-1.5 flex items-center gap-1 rounded text-[10px] font-medium text-slate-400 hover:text-slate-600 dark:text-slate-500 dark:hover:text-slate-300"
          >
            {expanded ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
            {expanded ? "Hide" : "Show"} {childCount} sub clone{childCount === 1 ? "" : "s"}
          </button>
        ) : null}
      </div>
    </div>
  );
}
