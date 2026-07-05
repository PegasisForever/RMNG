import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { ArrowRight, EllipsisVertical, Terminal } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import chatgptLogo from "../assets/chatgpt.png";
import claudeLogo from "../assets/claude.png";
import { buildSshCommand } from "~/lib/ssh";
import type { Host, Operation } from "~/lib/types";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { ForwardRuntime } from "~/lib/wire/ForwardRuntime";
import type { ForwardState } from "~/lib/wire/ForwardState";
import type { PortForward } from "~/lib/wire/PortForward";
import { workspaceBadge } from "~/lib/workspace";

// Text color + label per host state. `working` is sky, `idle` amber (done / awaiting
// the next task / needs you), `offline` rose. The state note carries the color; there
// is no longer a status dot (the unread dot took its place on the title row).
const AGENT_STATUS: Record<NonNullable<Host["monitorState"]>, { text: string; label: string }> = {
  working: { text: "text-sky-600 dark:text-sky-400", label: "working" },
  idle: { text: "text-amber-700 dark:text-amber-400", label: "idle" },
  offline: { text: "text-rose-500 dark:text-rose-400", label: "offline" },
};

function effectiveStatus(host: Host): { text: string; label: string } {
  return AGENT_STATUS[host.monitorState ?? "idle"];
}

/** How this clone's account for one provider was chosen + which account is actually in
 *  use, for a sidebar line. `mode` is the selection kind; `email` is the live account
 *  (absent for "none", or when auto/specific hasn't resolved one). Returns null when
 *  there's nothing to show (e.g. auto with no accounts configured). Provider-agnostic:
 *  the Claude and Codex lines both derive from this, off their respective host fields. */
type AcctSel = { mode: "auto" | "group" | "specific" | "none"; group?: string; email?: string };
function accountSelection(
  accountEmail: string | null | undefined,
  selection: string | null | undefined,
  group: string | null | undefined,
): AcctSel | null {
  const email = accountEmail || undefined;
  if (group) return { mode: "group", group, email };
  if (selection === "none") return { mode: "none" };
  if (selection === "auto") return email ? { mode: "auto", email } : null;
  if (selection && !selection.startsWith("group:")) return { mode: "specific", email: email ?? selection };
  // Legacy host (no selection recorded): show the account, without a mode badge.
  return email ? { mode: "specific", email } : null;
}

/** Short badge for the non-default selection modes (specific renders as the plain
 *  email, so it — and legacy hosts — get no badge). Group shows the group name. */
function selBadge(sel: AcctSel): string | null {
  if (sel.mode === "auto") return "auto";
  if (sel.mode === "none") return "none";
  if (sel.mode === "group") return sel.group ?? "group";
  return null; // specific
}

/** CPU (percent of the clone's cpu allowance) + memory-used strings, e.g.
 *  `{ cpu: "20%", mem: "3.2GB" }`. CPU rides the Claude line and MEM the Codex line;
 *  each renders in a fixed-width, right-aligned tabular slot so the two figures stack
 *  and line up across every row. CPU normalizes `stats.cpuPct` (docker convention:
 *  100 == one core) by `cloneCpus`; below 1% one decimal is kept so a near-idle clone
 *  doesn't read as dead-zero. When `cloneCpus <= 0` (unlimited clone) it falls back to a
 *  cores figure (`2.4c`). MEM is memory used in GiB, one decimal. Returns null when
 *  there's no usable sample — no stats yet, or a stopped/unmanaged host with no memory
 *  limit. `mem*` are typed bigint by ts-rs but arrive as JSON numbers, hence the
 *  `Number()` coercion. */
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

function selTitle(sel: AcctSel, provider: string): string {
  switch (sel.mode) {
    case "group":
      return `${provider} group: ${sel.group} (on ${sel.email ?? "?"})`;
    case "auto":
      return `${provider}: auto — server picks the best account${sel.email ? ` (on ${sel.email})` : ""}`;
    case "none":
      return `${provider}: none — no token installed`;
    default:
      return `${provider} account: ${sel.email} (fixed)`;
  }
}

/** One provider's metadata line: the account (logo · optional mode badge · email, which
 *  truncates) on the left, and an optional right-aligned usage metric (CPU or MEM) in a
 *  fixed-width tabular slot so the Claude and Codex figures align. `sel === null` renders
 *  just a left spacer — e.g. a Codex-less clone whose Codex line still carries MEM. */
function AccountLine({
  sel,
  logo,
  provider,
  metric,
}: {
  sel: AcctSel | null;
  logo: string;
  provider: string;
  metric?: { label: string; value: string; title: string };
}) {
  return (
    <div className="flex items-center gap-2 text-[10px]">
      {sel ? (
        <span
          className="flex min-w-0 flex-1 items-center gap-1 text-slate-400 dark:text-slate-500"
          title={selTitle(sel, provider)}
        >
          <img src={logo} alt={provider} className="h-3 w-3 shrink-0 object-contain" />
          {selBadge(sel) ? (
            <span className="shrink-0 rounded bg-slate-100 px-1 text-[9px] font-semibold text-slate-500 dark:bg-slate-800 dark:text-slate-400">
              {selBadge(sel)}
            </span>
          ) : null}
          {sel.email ? (
            <span className="truncate">{sel.email}</span>
          ) : sel.mode === "none" ? (
            <span className="italic text-slate-300 dark:text-slate-600">no token</span>
          ) : null}
        </span>
      ) : (
        <span className="min-w-0 flex-1" />
      )}
      {metric ? (
        <span className="flex shrink-0 items-baseline gap-1 tabular-nums" title={metric.title}>
          <span className="font-medium text-slate-400 dark:text-slate-500">{metric.label}</span>
          <span className="w-8 text-right font-semibold text-slate-700 dark:text-slate-200">
            {metric.value}
          </span>
        </span>
      ) : null}
    </div>
  );
}

// Status dot per forward state (+ a muted "disabled" for rules toggled off), shown in
// the compact per-host forwards chips.
const FORWARD_DOT: Record<ForwardState | "disabled", string> = {
  listening: "bg-emerald-500",
  error: "bg-red-500",
  offline: "bg-slate-400 dark:bg-slate-500",
  disabled: "bg-slate-300 dark:bg-slate-600",
};

/** A compact wrapping row of this host's port forwards — one `remote→local` chip per
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
  /** Open the port-forward editor for this host. */
  onPortForward: () => void;
  /** Live runtime status for this host's forwards (from the `forwards` SSE event),
   *  merged into the compact forwards chips by rule id. */
  forwardRuntime?: ForwardRuntime[];
  /** `ssh.publicHost` (config) — the `-J` jump target for the copied command. Empty ⇒
   *  falls back to `window.location.hostname` (this page's own address). */
  sshPublicHost: string;
  /** `listen.bastion` — the bastion `sshd` port the copied command jumps through. */
  bastionPort: number;
}

/** A single overflow-menu item that copies `command` to the clipboard and shows a
 *  brief "Copied!" label before asking the menu to close. Kept separate from the
 *  plain-text `item()` helper because it needs its own transient state + delayed
 *  close (the other items close immediately on click). */
function CopySshMenuItem({ command, onDone }: { command: string; onDone: () => void }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      role="menuitem"
      onPointerDown={(e) => e.stopPropagation()}
      onClick={async (e) => {
        e.stopPropagation();
        try {
          await navigator.clipboard.writeText(command);
        } catch {
          // Clipboard write failed (e.g. insecure context) — still close normally;
          // the command is short enough to retype from the CLI (`rmng ssh <clone>`).
        }
        setCopied(true);
        setTimeout(onDone, 900);
      }}
      title={command}
      className="flex w-full items-center gap-1.5 px-3 py-1.5 text-left text-xs text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-700"
    >
      <Terminal className="size-4 shrink-0" />
      {copied ? "Copied!" : "Copy SSH command"}
    </button>
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
  onPortForward,
  onDelete,
  sshCommand,
}: {
  hostId: string;
  managed: boolean;
  busy: boolean;
  onCommit: () => void;
  onChangeAccount: () => void;
  onPortForward: () => void;
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
        aria-label={`actions for ${hostId}`}
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
          {managed ? (
            <>
              {item("Commit to image…", onCommit)}
              {item("Change account…", onChangeAccount)}
              {item("Port forward…", onPortForward)}
              {sshCommand ? <CopySshMenuItem command={sshCommand} onDone={() => setOpen(false)} /> : null}
              <div className="my-1 h-px bg-slate-100 dark:bg-slate-700" />
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
  onPortForward,
  forwardRuntime,
  sshPublicHost,
  bastionPort,
}: SidebarHostProps) {
  const busy = op?.status === "running";
  // Managed clones (backed by a container named after the host id) get the commit /
  // account actions; plain unmanaged rows only get remove.
  const managed = host.managed === true;
  // Only managed clones run a real sshd to jump into (Task 7/8 provisioning) — an
  // unmanaged row has no container, so no command is offered for it.
  const sshCommand = managed
    ? buildSshCommand(sshPublicHost || window.location.hostname, bastionPort, host.id)
    : undefined;
  const status = effectiveStatus(host);
  const claudeSel = accountSelection(host.claudeAccountEmail, host.claudeSelection, host.claudeGroup);
  const codexSel = accountSelection(host.codexAccountEmail, host.codexSelection, host.codexGroup);
  const usage = usageParts(stats, cloneCpus);
  // CPU rides the Claude line, MEM the Codex line, so the two figures stack on the right
  // across both lines. They share `usage`, so they appear and vanish together.
  const cpuMetric = usage
    ? { label: "CPU", value: usage.cpu, title: "live container CPU (% of clone allowance)" }
    : undefined;
  const memMetric = usage
    ? { label: "MEM", value: usage.mem, title: "container memory used" }
    : undefined;
  // Show a provider line when it has an account or its metric; drop it when empty so a
  // Codex-less, stat-less clone doesn't sprout a blank row.
  const showClaudeLine = !!claudeSel || !!cpuMetric;
  const showCodexLine = !!codexSel || !!memMetric;
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
      className={`group flex touch-none cursor-grab items-start gap-1 border-b border-b-slate-200 border-l-2 border-l-transparent px-1.5 py-1.5 last:border-b-0 active:cursor-grabbing dark:border-b-slate-700 ${
        // Per-side borders (explicit colors so they never collide): a slate-200 bottom
        // divider between rows + a left accent for the selected row. Exactly one
        // background wins (dragging ▸ selected ▸ default); the default is a solid
        // slate-50 (not transparent) so a dragged card fully hides the rows under it.
        // While dragging the card lifts out as a rounded, divider-less floating card.
        isDragging
          ? "rounded-md border-b-transparent bg-white shadow-lg ring-1 ring-slate-300 dark:bg-slate-800 dark:ring-slate-600"
          : selected
            ? "border-l-emerald-400 bg-emerald-50 dark:bg-emerald-950"
            : "bg-slate-50 hover:bg-slate-100 dark:bg-slate-900 dark:hover:bg-slate-800"
      }`}
    >
      <div className="min-w-0 flex-1">
        {/* Top block: the Claude line and, under it, the Codex line — each an account on
            the left with its usage figure (CPU on Claude, MEM on Codex) right-aligned in
            a shared tabular slot. The ⋯ menu sits to the right, vertically centered so it
            spans both lines. While busy, the op step replaces the two lines. */}
        <div className="mb-0.5 flex items-center gap-1">
          <div className="min-w-0 flex-1">
            {busy ? (
              <div className="flex items-center gap-2">
                <span className="min-w-0 flex-1 break-words text-sm font-medium text-slate-800 dark:text-slate-100">
                  {host.displayName ?? host.id}
                </span>
                <span className="shrink-0 text-[10px] font-medium text-sky-600 dark:text-sky-400">
                  {op?.kind === "delete" ? "deleting…" : op?.step}
                </span>
              </div>
            ) : (
              <>
                {showClaudeLine ? (
                  <AccountLine sel={claudeSel} logo={claudeLogo} provider="Claude" metric={cpuMetric} />
                ) : null}
                {showCodexLine ? (
                  <AccountLine sel={codexSel} logo={chatgptLogo} provider="Codex" metric={memMetric} />
                ) : null}
              </>
            )}
          </div>
          <OverflowMenu
            hostId={host.id}
            managed={managed}
            busy={busy}
            onCommit={onCommit}
            onChangeAccount={onChangeAccount}
            onPortForward={onPortForward}
            onDelete={onDelete}
            sshCommand={sshCommand}
          />
        </div>

        {/* Title: unread "!" mark + ticket badge inlined with the title, so a wrapped title
            flows back to the left edge on the next line (the badge doesn't indent it).
            Hidden while busy — the op step shows in the top block instead. */}
        {!busy ? (
          <p className="break-words text-sm font-medium leading-snug text-slate-800 dark:text-slate-100">
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
        ) : null}

        {/* Agent state note (or status label fallback), colored by status. */}
        {!busy ? (
          <p
            className={`mt-1 line-clamp-2 text-xs leading-snug ${status.text}`}
            title={host.stateNote || status.label}
          >
            {[host.linearLabel, host.stateNote || status.label].filter(Boolean).join(" · ")}
          </p>
        ) : null}

        {/* Compact list of this host's port forwards (remote→local, live status dot). */}
        {!busy && host.forwards && host.forwards.length > 0 ? (
          <ForwardChips forwards={host.forwards} runtime={forwardRuntime ?? []} />
        ) : null}
      </div>
    </div>
  );
}
