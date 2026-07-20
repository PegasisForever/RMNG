import { useEffect, useState } from "react";

import { AccountGroupSelect, NO_GROUP } from "~/components/AccountGroupSelect";
import { ImagePicker } from "~/components/ImagePicker";
import { getConfig, type ClonePayload } from "~/lib/api";
import type { Group } from "~/lib/wire/Group";
import type { ImageInfo } from "~/lib/wire/ImageInfo";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
import { parseTicketInput, workspaceBadge } from "~/lib/workspace";

/**
 * Clone dialog. Pick a clone-source image, then one of three ticket modes: paste an
 * existing Linear ticket (link or `WE-142`) — the preset is auto-selected from the
 * ticket-id prefix unless overridden; create a new ticket (preset + team + title +
 * description — the preset's Linear key creates it); or a plain no-ticket clone
 * (title + optional first message; a preset must be picked when any are configured).
 * The hostname derives from the ticket id (`WE-142` → `pega-we-142`) or the title
 * slug. All resolved server-side.
 */
export function CloneModal({
  images,
  imagesLoading,
  busy,
  onClose,
  onClone,
}: {
  /** Clone-source images to pick from (from `listImages`). */
  images: ImageInfo[];
  imagesLoading: boolean;
  busy: boolean;
  onClose: () => void;
  /** `image` = the chosen clone-source image reference. */
  onClone: (image: string, payload: ClonePayload) => void;
}) {
  const [image, setImage] = useState<string | null>(null);
  const [mode, setMode] = useState<"existing" | "create" | "plain">("existing");
  const [ticket, setTicket] = useState("");
  // Linear team key for created tickets (e.g. "we" → WE-…).
  const [team, setTeam] = useState("");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [message, setMessage] = useState("");
  const [agentInstructions, setAgentInstructions] = useState("");
  const [claudeInstructions, setClaudeInstructions] = useState("");
  // The account group this clone binds to (a CLIProxyAPI pool), or "none" for no
  // inference. Defaults to the first configured group once config loads, else "none".
  const [group, setGroup] = useState(NO_GROUP);
  // Account groups (from config), for the picker options.
  const [groups, setGroups] = useState<Group[]>([]);
  // Presets (from config) + the chosen one ("" = auto-by-ticket-prefix; create/plain
  // require an explicit pick, defaulted to the first preset below).
  const [presets, setPresets] = useState<PresetRedacted[]>([]);
  const [preset, setPreset] = useState("");

  useEffect(() => {
    getConfig()
      .then((c) => {
        setPresets(c.presets);
        setGroups(c.groups);
        if (c.groups.length > 0) setGroup(c.groups[0].name);
      })
      .catch(() => {
        // Config unreachable — just no preset/group options.
      });
  }, []);

  // Create/plain mode need an explicit preset — default to the first one; back on
  // the ticket tab "" means auto-by-prefix, so leave whatever the operator chose.
  useEffect(() => {
    if (mode !== "existing" && preset === "" && presets.length > 0) {
      setPreset(presets[0].name);
    }
  }, [mode, presets, preset]);

  const parsed = parseTicketInput(ticket);
  // A source image is always required; then: `existing` needs a parseable ticket;
  // `create` a preset (its key creates the ticket) + team + title; `plain` a title +
  // a preset whenever any are configured.
  const modeValid =
    mode === "existing"
      ? !!parsed
      : mode === "create"
        ? title.trim().length > 0 && team.trim().length > 0 && !!preset
        : title.trim().length > 0 && (presets.length === 0 || !!preset);
  const valid = !!image && modeValid;

  // The clone request's group binding: a group name, or null for "none".
  const groupValue = group === NO_GROUP ? null : group;

  function submit() {
    if (!valid || busy || !image) return;
    if (mode === "plain") {
      // No ticket: just a title and an optional first message (empty ⇒ no auto-send).
      onClone(image, {
        plain: { title: title.trim(), message: message.trim() },
        group: groupValue,
        preset: preset || undefined,
      });
      return;
    }
    const extra: { agentInstructions?: string; claudeInstructions?: string } =
      {};
    if (agentInstructions.trim())
      extra.agentInstructions = agentInstructions.trim();
    if (claudeInstructions.trim())
      extra.claudeInstructions = claudeInstructions.trim();
    if (mode === "existing")
      onClone(image, {
        ticket: ticket.trim(),
        ...extra,
        group: groupValue,
        preset: preset || undefined, // "" ⇒ auto-select by ticket-id prefix
      });
    else
      onClone(image, {
        create: { team: team.trim().toLowerCase(), title: title.trim(), description },
        ...extra,
        group: groupValue,
        preset: preset || undefined,
      });
  }

  const tab = (m: typeof mode, label: string) => (
    <button
      type="button"
      onClick={() => setMode(m)}
      className={`flex-1 rounded px-2 py-1 ${
        mode === m
          ? "bg-white text-slate-900 shadow-sm dark:bg-slate-700 dark:text-slate-100"
          : "text-slate-500 hover:text-slate-700 dark:text-slate-400 dark:hover:text-slate-200"
      }`}
    >
      {label}
    </button>
  );

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4"
      onClick={onClose}
    >
      <div
        className="max-h-[90vh] w-full max-w-md overflow-y-auto rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">New clone</h3>

        <div className="mt-3 text-xs font-medium text-slate-500 dark:text-slate-400">
          Source image
          <ImagePicker
            images={images}
            loading={imagesLoading}
            value={image}
            onChange={setImage}
          />
        </div>

        <div className="mt-3 flex gap-0.5 rounded-md bg-slate-100 p-0.5 text-xs font-medium dark:bg-slate-800">
          {tab("existing", "Existing ticket")}
          {tab("create", "New ticket")}
          {tab("plain", "No ticket")}
        </div>

        {mode === "existing" ? (
          <label className="mt-3 block text-xs font-medium text-slate-500 dark:text-slate-400">
            Linear ticket link or id
            <input
              autoFocus
              value={ticket}
              onChange={(e) => setTicket(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") submit();
              }}
              placeholder="https://linear.app/…/issue/WE-142  or  WE-142"
              spellCheck={false}
              className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500"
            />
            {ticket && !parsed ? (
              <p className="mt-1 text-[11px] text-red-600 dark:text-red-400">
                couldn’t find a ticket id (like WE-142) in that
              </p>
            ) : null}
            {parsed ? (
              <p className="mt-1.5 flex items-center gap-1.5 text-[11px] font-normal text-slate-500 dark:text-slate-400">
                <span
                  className={`rounded px-1.5 py-0.5 font-medium ${workspaceBadge(parsed.prefix)}`}
                >
                  {parsed.identifier}
                </span>
                <span aria-hidden>→</span>
                <span className="font-mono text-slate-700 dark:text-slate-200">
                  {parsed.hostname}
                </span>
              </p>
            ) : null}
          </label>
        ) : mode === "create" ? (
          <div className="mt-3 space-y-3">
            <label className="block text-xs font-medium text-slate-500 dark:text-slate-400">
              Team key
              <input
                value={team}
                onChange={(e) => setTeam(e.target.value)}
                placeholder="we"
                spellCheck={false}
                className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500"
              />
              <span className="mt-0.5 block text-[11px] font-normal text-slate-400 dark:text-slate-500">
                The Linear team the ticket is created in (WE-… → “we”), using the
                selected preset’s API key.
              </span>
            </label>
            <label className="block text-xs font-medium text-slate-500 dark:text-slate-400">
              Title
              <input
                autoFocus
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submit();
                }}
                placeholder="Short ticket title"
                className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500"
              />
            </label>
            <label className="block text-xs font-medium text-slate-500 dark:text-slate-400">
              Description
              <textarea
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                rows={3}
                placeholder="Optional — what needs doing"
                className="mt-1 w-full resize-y rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500"
              />
            </label>
          </div>
        ) : (
          <div className="mt-3 space-y-3">
            <label className="block text-xs font-medium text-slate-500 dark:text-slate-400">
              Title
              <input
                autoFocus
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submit();
                }}
                placeholder="Container title"
                className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500"
              />
            </label>
            <label className="block text-xs font-medium text-slate-500 dark:text-slate-400">
              First message to the agent
              <textarea
                value={message}
                onChange={(e) => setMessage(e.target.value)}
                rows={3}
                placeholder="Optional — leave empty to not auto-send a first message"
                className="mt-1 w-full resize-y rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500"
              />
            </label>
          </div>
        )}

        {groups.length > 0 ? (
          <label className="mt-3 block text-xs font-medium text-slate-500 dark:text-slate-400">
            Account group
            <AccountGroupSelect
              groups={groups}
              value={group}
              onChange={setGroup}
              className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100"
            />
            <span className="mt-0.5 block text-[11px] font-normal text-slate-400 dark:text-slate-500">
              The account pool this clone's agents route through. Its CLIProxyAPI instance
              owns account selection + failover.
            </span>
          </label>
        ) : null}

        {presets.length > 0 ? (
          <label className="mt-3 block text-xs font-medium text-slate-500 dark:text-slate-400">
            Preset
            <select
              value={preset}
              onChange={(e) => setPreset(e.target.value)}
              className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100"
            >
              {mode === "existing" ? (
                <option value="">Auto (from ticket-id prefix)</option>
              ) : null}
              {presets.map((p) => (
                <option key={p.name} value={p.name}>
                  {p.name}
                  {p.labels.length > 0 ? ` · ${p.labels.join(", ")}` : ""} (
                  {p.vars.length} var{p.vars.length === 1 ? "" : "s"})
                </option>
              ))}
            </select>
          </label>
        ) : mode === "create" ? (
          <p className="mt-3 text-[11px] text-red-600 dark:text-red-400">
            Creating a ticket needs a preset with a Linear API key — add one in
            Settings.
          </p>
        ) : null}

        {mode !== "plain" ? (
          <details className="mt-3 text-xs">
            <summary className="cursor-pointer font-medium text-slate-500 hover:text-slate-700 dark:text-slate-400 dark:hover:text-slate-200">
              Instruction overrides (optional)
            </summary>
            <div className="mt-2 space-y-3">
              <p className="text-[11px] font-normal text-slate-400 dark:text-slate-500">
                Added on top of the defaults and marked as taking precedence —
                the agent acts on the merged instruction.
              </p>
              <label className="block font-medium text-slate-500 dark:text-slate-400">
                Host agent instructions
                <textarea
                  value={agentInstructions}
                  onChange={(e) => setAgentInstructions(e.target.value)}
                  rows={2}
                  placeholder={
                    'Appended to the default ("Follow your \"Implementing a ticket\" procedure"); takes precedence where they conflict.'
                  }
                  className="mt-1 w-full resize-y rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500"
                />
              </label>
              <label className="block font-medium text-slate-500 dark:text-slate-400">
                Claude Code instructions
                <textarea
                  value={claudeInstructions}
                  onChange={(e) => setClaudeInstructions(e.target.value)}
                  rows={3}
                  placeholder={
                    "Appended to the default (pull latest → switch to the feature branch → setup docs → implement); takes precedence where they conflict."
                  }
                  className="mt-1 w-full resize-y rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 dark:bg-slate-800 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:text-slate-100 dark:placeholder:text-slate-500"
                />
              </label>
            </div>
          </details>
        ) : null}

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
            disabled={!valid || busy}
            className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
          >
            {mode === "create" ? "Create & clone" : "Clone"}
          </button>
        </div>
      </div>
    </div>
  );
}
