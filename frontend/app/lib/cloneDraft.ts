// The clone dialog's model: the form, the config it draws from, the operation it started,
// and every rule that reads them. No React and no network here — the container feeds it
// events and renders what comes back, so each rule is a test. The one thing read from outside
// is the team a new ticket starts on, which is remembered in storage.

import { startingTeam } from "~/lib/linear/intake";
import type { Operation } from "~/lib/types";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { CloneRequest } from "~/lib/wire/CloneRequest";
import type { LinearMeta } from "~/lib/wire/LinearMeta";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
import { parseTicketInput } from "~/lib/workspace";

/** Which of the dialog's four tabs is open. The first three fork a live clone; the fourth
 *  builds one from a preset image onto a fresh home. */
export type CloneMode = "existing" | "create" | "plain" | "template";

/** Everything the operator can type or pick. */
export interface CloneDraft {
 /** Source clone to fork; null until the picker settles. The template tab forks nothing. */
 source: string | null;
 mode: CloneMode;
 /** Existing-ticket tab: a Linear link or a bare `WE-142`. */
 ticket: string;
 /** New-ticket tab: the Linear team key, lowercase. */
 team: string;
 /** The clone's title. */
 title: string;
 /** New-ticket tab: the ticket body, as markdown, written by the editor slot. */
 description: string;
 /** New-ticket tab: Linear's own priority, 0 unranked through 4 low. */
 priority: number;
 /** No-ticket tab: an optional first message to the agent. */
 message: string;
 /** Ticket tabs only: appended to the agent's and Claude Code's default instructions. */
 agentInstructions: string;
 claudeInstructions: string;
 /** Account picks: an email pins, `auto` rotates inside the pool. The resolved preset
  *  fills both until the operator picks one by hand. */
 claudeAccount: string;
 codexAccount: string;
 /** The account pool: a name binds, `none` unbinds to every pool. Filled from the preset
  *  the same way; blank only before one resolves, where the server decides. */
 group: string;
 /** No-ticket tab: the hand-picked preset. */
 plainPreset: string;
 /** Template tab: the hand-picked preset whose Dockerfile builds the image. */
 templatePreset: string;
 /** Headless clone: no desktop, so the viewer shows a tmux tab view. */
 headless: boolean;
 /** Force a fresh image build with a fresh base pull. Off unless checked. */
 rebuild: boolean;
 /** Run the preset's startup script as the clone user. On unless unchecked. */
 runStartupScript: boolean;
}

/** The form as the dialog opens it. `ticket` is seeded when something opened it with a
 *  ticket in hand (a card dragged onto a column, or a ticket's own menu). */
export function emptyCloneDraft(ticket = ""): CloneDraft {
 return {
  source: null,
  mode: "existing",
  ticket,
  team: "",
  title: "",
  description: "",
  priority: 0,
  message: "",
  agentInstructions: "",
  claudeInstructions: "",
  claudeAccount: "",
  codexAccount: "",
  group: "",
  plainPreset: "",
  templatePreset: "",
  headless: false,
  rebuild: false,
  runStartupScript: true,
 };
}

/** One team key, with the preset that claims it. */
export interface TeamKey {
 key: string;
 preset: PresetRedacted;
}

/** Every distinct team key across the presets' labels, each mapped to the preset that claims
 *  it — the first in config order, mirroring the server's `pick_preset_by_prefix`. This is the
 *  new-ticket tab's team dropdown AND its preset selector: they are the same choice. */
export function teamKeysOf(presets: PresetRedacted[]): TeamKey[] {
 const seen = new Map<string, PresetRedacted>();
 for (const p of presets) {
  for (const label of p.labels) {
   const key = label.toLowerCase();
   if (!seen.has(key)) seen.set(key, p);
  }
 }
 return [...seen.entries()].map(([key, preset]) => ({ key, preset }));
}

/**
 * The preset that will drive the clone, per tab.
 *
 * - `plain` / `template`: whatever the operator picked by hand.
 * - `create`: implied by the chosen team key, which is why that tab has no preset dropdown.
 * - `existing`: the first preset whose label matches the ticket-id prefix.
 */
export function resolvePreset(
 mode: CloneMode,
 presets: PresetRedacted[],
 {
  plainPreset,
  templatePreset,
  team,
  ticketPrefix,
 }: {
  plainPreset?: string;
  templatePreset?: string;
  team?: string;
  ticketPrefix?: string;
 },
): PresetRedacted | undefined {
 if (mode === "plain") return presets.find((p) => p.name === plainPreset);
 if (mode === "template") return presets.find((p) => p.name === templatePreset);
 const wanted = mode === "create" ? team?.toLowerCase() : ticketPrefix;
 return wanted
  ? presets.find((p) => p.labels.some((l) => l.toLowerCase() === wanted))
  : undefined;
}

// --- the dialog, as one model -------------------------------------------------------------

/** The picks the follow rules below leave alone once made by hand. */
const FOLLOWED = ["source", "group", "claudeAccount", "codexAccount"] as const;
type Followed = (typeof FOLLOWED)[number];

export interface CloneDialog {
 draft: CloneDraft;
 touched: ReadonlySet<Followed>;
 presets: PresetRedacted[];
 groups: CloneGroup[];
 /** Config settled (loaded or failed). Empty presets before that are indistinguishable
  *  from none configured, which would flash the missing-key warning on every open. */
 configLoaded: boolean;
 /** Forkable clone ids, oldest first. */
 sources: string[];
 /** The started operation, once the POST answers. */
 opId: string | null;
 starting: boolean;
 seen: boolean;
 failed: boolean;
 /** The operation settled well: the dialog may close. */
 done: boolean;
 error: string | null;
}

export type CloneDialogEvent =
 | { type: "config"; presets: PresetRedacted[]; groups: CloneGroup[] }
 | { type: "sources"; ids: string[] }
 | {
    [K in keyof CloneDraft]: { type: "edit"; key: K; value: CloneDraft[K] };
   }[keyof CloneDraft]
 | { type: "starting" }
 | { type: "started"; opId: string }
 | { type: "failed"; message: string }
 | { type: "op"; op: Operation | undefined };

export function emptyCloneDialog(
 ticket = "",
 source: string | null = null,
): CloneDialog {
 return {
  draft: { ...emptyCloneDraft(ticket), source },
  // A source handed in (from a clone's own menu) counts as the operator's own pick.
  touched: new Set(source ? (["source"] as Followed[]) : []),
  presets: [],
  groups: [],
  configLoaded: false,
  sources: [],
  opId: null,
  starting: false,
  seen: false,
  failed: false,
  done: false,
  error: null,
 };
}

export function cloneDialogReducer(
 s: CloneDialog,
 e: CloneDialogEvent,
): CloneDialog {
 switch (e.type) {
  case "config":
   return follow({
    ...s,
    presets: e.presets,
    groups: e.groups,
    configLoaded: true,
   });
  case "sources":
   // Nothing to fork: the template tab is the only one that can still make a clone.
   return follow({
    ...s,
    sources: e.ids,
    draft: e.ids.length === 0 ? { ...s.draft, mode: "template" } : s.draft,
   });
  case "edit":
   return follow({
    ...s,
    draft: { ...s.draft, [e.key]: e.value },
    touched: (FOLLOWED as readonly string[]).includes(e.key)
     ? new Set([...s.touched, e.key as Followed])
     : s.touched,
   });
  case "starting":
   // Clear the last attempt so a retry tracks the new op, not the failed one still
   // sitting in the list for another minute.
   return {
    ...s,
    starting: true,
    opId: null,
    seen: false,
    failed: false,
    done: false,
    error: null,
   };
  case "started":
   return { ...s, starting: false, opId: e.opId };
  case "failed":
   return { ...s, starting: false, error: e.message };
  case "op": {
   const seen = s.seen || !!e.op;
   const failed = s.failed || e.op?.status === "error";
   const kind = s.draft.mode === "template" ? "clone" : "fork";
   return {
    ...s,
    seen,
    failed,
    error:
     failed && !s.failed ? e.op?.message || `the ${kind} failed` : s.error,
    done: opPhase(e.op, seen, failed) === "done",
   };
  }
 }
}

/** Fill in what the operator has not: the tab's preset and team, the fork source, and the
 *  pool and both accounts the resolved preset names. A pick made by hand stays put — unless
 *  it stopped qualifying, as a source clone does when it is deleted or archived. */
function follow(s: CloneDialog): CloneDialog {
 let d = s.draft;
 const first = s.presets[0]?.name ?? "";
 if (d.mode === "plain" && d.plainPreset === "")
  d = { ...d, plainPreset: first };
 if (d.mode === "template" && d.templatePreset === "")
  d = { ...d, templatePreset: first };
 if (d.mode === "create" && d.team === "")
  d = { ...d, team: startingTeam(teamKeysOf(s.presets)) };

 let touched = s.touched;
 const preset = presetOf({ ...s, draft: d });
 if (s.configLoaded && s.sources.length > 0 && d.mode !== "template") {
  if (touched.has("source") && !(d.source && s.sources.includes(d.source))) {
   touched = new Set([...touched].filter((k) => k !== "source"));
  }
  if (!touched.has("source")) {
   const wanted = preset?.defaultForkClone.trim();
   // Blank until a preset resolves; with none configured there is nothing to wait for.
   d = {
    ...d,
    source:
     !preset && s.presets.length > 0
      ? null
      : wanted && s.sources.includes(wanted)
        ? wanted
        : s.sources[0],
   };
  }
 }
 if (s.configLoaded && preset) {
  if (!touched.has("group")) {
   const g = preset.group.trim();
   d = { ...d, group: g === "" || g.toLowerCase() === "none" ? "none" : g };
  }
  if (!touched.has("claudeAccount")) d = { ...d, claudeAccount: "auto" };
  if (!touched.has("codexAccount")) d = { ...d, codexAccount: "auto" };
 }
 return { ...s, draft: d, touched };
}

/** The preset the open tab will actually use. */
export function presetOf(s: CloneDialog): PresetRedacted | undefined {
 const d = s.draft;
 return resolvePreset(d.mode, s.presets, {
  plainPreset: d.plainPreset,
  templatePreset: d.templatePreset,
  team: d.team,
  ticketPrefix: parseTicketInput(d.ticket)?.prefix,
 });
}

/** This tab needs a Linear key nobody configured. `create` opens the issue with the resolved
 *  preset's own key; `existing` only looks one up, and every key is tried in turn. */
export function linearKeyMissing(s: CloneDialog): boolean {
 const mode = s.draft.mode;
 if (!s.configLoaded || mode === "plain" || mode === "template") return false;
 if (mode === "create") return !presetOf(s)?.linearKey;
 return !s.presets.some((p) => p.linearKey !== "");
}

/** Whether the Create button may fire. */
export function cloneDialogValid(s: CloneDialog): boolean {
 const d = s.draft;
 const titled = d.title.trim().length > 0;
 // With the preset dropdown gone from the ticket tabs, a prefix no preset claims is a
 // request the server would refuse.
 const picked = s.presets.length === 0 || !!presetOf(s);
 const ok =
  d.mode === "existing"
   ? !!parseTicketInput(d.ticket) && picked
   : d.mode === "create"
     ? titled && d.team.trim().length > 0
     : titled && picked;
 return ok && (d.mode === "template" || !!d.source) && !linearKeyMissing(s);
}

/** A clone is being started, or one is running: the form and both buttons lock. */
export function cloneDialogBusy(s: CloneDialog): boolean {
 return s.starting || (!!s.opId && !s.failed);
}

/** What this tab would send. `linear` is Linear's own answer on the ticket tabs; the other
 *  two carry the typed title, which is what names the clone. */
export function cloneRequest(
 s: CloneDialog,
 linear?: LinearMeta,
): CloneRequest {
 const d = s.draft;
 const ticketTab = d.mode === "existing" || d.mode === "create";
 return {
  source: d.mode === "template" ? undefined : (d.source ?? undefined),
  preset: presetOf(s)?.name,
  linear: linear ?? { displayName: d.title.trim() },
  group: d.group || undefined,
  claudeAccount: d.claudeAccount || undefined,
  codexAccount: d.codexAccount || undefined,
  firstMessage: d.mode === "plain" ? d.message.trim() || undefined : undefined,
  agentInstructions: ticketTab
   ? d.agentInstructions.trim() || undefined
   : undefined,
  claudeInstructions: ticketTab
   ? d.claudeInstructions.trim() || undefined
   : undefined,
  headless: d.headless,
  runStartupScript: d.runStartupScript,
  rebuild: d.rebuild,
 };
}

/** What a dialog should do about the operation it started. */
export type OpPhase = "running" | "done" | "failed";

/**
 * Finished operations are pruned from state shortly after they settle (8s after Done, 60s
 * after Error), so a poll can miss the terminal frame: **an op that vanished after being
 * seen counts as done**, the same rule the CLI's waiter uses. `alreadyFailed` is sticky,
 * because that rule would otherwise close a dialog over its own error message.
 */
export function opPhase(
 op: Operation | undefined,
 everSeen: boolean,
 alreadyFailed: boolean,
): OpPhase {
 if (alreadyFailed || op?.status === "error") return "failed";
 if (op?.status === "done") return "done";
 if (!op && everSeen) return "done";
 return "running";
}
