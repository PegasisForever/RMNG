// The clone dialog's form model, and every rule that reads it. No React, no network: the
// container holds a `CloneDraft` in state and the View renders one, so both sides agree on
// what "the form" is, and a story can build one with `makeCloneDraft`.
//
// The rules here mirror the server, which is why they are worth keeping in one place: the
// dialog blocks exactly the requests the server would reject, and says which of them it is.

import type { Operation } from "~/lib/types";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

/** Which of the dialog's four tabs is open. Each one builds a different clone request:
 *  the first three fork a live source clone, the fourth creates from a template image
 *  onto a fresh empty home dataset. */
export type CloneMode = "existing" | "create" | "plain" | "template";

/** Everything the operator can type or pick in the dialog. One editable model, edited through
 *  a single `updateField`, so the View takes two props for the form instead of thirty. */
export interface CloneDraft {
  /** Source clone id to fork; null until the picker settles on one. Only the first
   *  three tabs fork — the template tab creates from an image and leaves this null. */
  source: string | null;
  mode: CloneMode;
  /** Existing-ticket tab: a Linear link or a bare `WE-142`. */
  ticket: string;
  /** New-ticket tab: the Linear team key, lowercase. Picked from the presets' own labels. */
  team: string;
  /** New-ticket and no-ticket tabs. */
  title: string;
  /** New-ticket tab: the ticket body, as markdown. Written by the editor in the description
   *  slot rather than by a field of the form. */
  description: string;
  /** New-ticket tab: Linear's own priority, 0 unranked through 4 low. There is no assignee
   *  beside it: a clone is work you are about to start, so the issue is yours. */
  priority: number;
  /** No-ticket tab: an optional first message to the agent. */
  message: string;
  /** Ticket tabs only: appended to the clone agent's and Claude Code's default instructions. */
  agentInstructions: string;
  claudeInstructions: string;
  /** Account-pool OVERRIDES. "" = follow the resolved preset's default (the server resolves
   *  it). A non-empty value pins the clone to that pool regardless of preset.
   *
   *  Seeding these to `auto` would put the preset's configured default out of reach: the
   *  server's chain takes an explicit request over the preset, and `auto` IS explicit, so
   *  every clone made here would silently override the preset. */
  claudeAccount: string;
  codexAccount: string;
  /** Pool OVERRIDE, same shape as the account overrides: "" follows the source (omit),
   *  "none" unbinds to any-group scope (send null), a name binds. Groups are offered but
   *  never required. */
  group: string;
  /** No-ticket tab: the hand-picked preset. The ticket tabs never pick one by hand. */
  plainPreset: string;
  /** Template tab: the hand-picked preset whose Dockerfile builds the image. */
  templatePreset: string;
  /** Headless clone: no desktop, so the viewer shows a tmux tab view instead of a stream. */
  headless: boolean;
  /** Run the preset's startup script as the clone user. On unless unchecked. */
  runStartupScript: boolean;
}

/** The form as the dialog opens it. `ticket` is seeded when something opened the dialog with
 *  a ticket in hand (a card dragged onto a column, or a ticket's own menu). */
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
    runStartupScript: true,
  };
}

/** One team key, with the preset that claims it. */
export interface TeamKey {
  key: string;
  preset: PresetRedacted;
}

/** Every distinct team key across the presets' labels, each mapped to the preset that claims
 *  it — the first one in config order, mirroring the server's `pick_preset_by_prefix`. This is
 *  the new-ticket tab's team dropdown AND its preset selector: they are the same choice. */
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
 * The preset that will actually drive the clone, per tab — mirroring what the server does so
 * the dialog shows the truth rather than a guess.
 *
 * - `plain` / `template`: whatever the operator picked by hand.
 * - `create`: implied by the chosen team key. The key comes from the presets' own labels, so
 *   picking a team IS picking a preset — which is why that tab has no preset dropdown.
 * - `existing`: auto-selected from the ticket-id prefix, mirroring the server's
 *   `pick_preset_by_prefix` (first preset in config order with a case-insensitively matching
 *   label). Undefined until a ticket parses, so the group control reads blank until then.
 */
export function resolvePreset(
  mode: CloneMode,
  presets: PresetRedacted[],
  { plainPreset, templatePreset, team, ticketPrefix }: {
    plainPreset?: string;
    templatePreset?: string;
    team?: string;
    ticketPrefix?: string;
  },
): PresetRedacted | undefined {
  if (mode === "plain") return presets.find((p) => p.name === plainPreset);
  if (mode === "template") return presets.find((p) => p.name === templatePreset);
  if (mode === "create") {
    return team
      ? presets.find((p) => p.labels.some((l) => l.toLowerCase() === team.toLowerCase()))
      : undefined;
  }
  return ticketPrefix
    ? presets.find((p) => p.labels.some((l) => l.toLowerCase() === ticketPrefix))
    : undefined;
}

/**
 * Whether the request this tab would send needs a Linear API key nobody has configured.
 *
 * Both ticket modes need one, but not the same one — mirror the server so the dialog blocks
 * exactly the requests it would reject. `create` opens the issue with the *resolved* preset's
 * key (`resolve_issue`), so that one preset must have it; `existing` only fetches, and the
 * server tries every preset's key in turn (`fetch_issue_any`), so any one of them will do.
 * `plain` and `template` never touch Linear.
 *
 * `configLoaded` is separate from "no presets": `presets` starts empty while the config is in
 * flight, which is indistinguishable from "none configured", and without the gate the warning
 * flashes on every open.
 */
export function linearKeyMissing(
  mode: CloneMode,
  presets: PresetRedacted[],
  preset: PresetRedacted | undefined,
  configLoaded: boolean,
): boolean {
  if (!configLoaded || mode === "plain" || mode === "template") return false;
  if (mode === "create") return !preset?.linearKey;
  return !presets.some((p) => p.linearKey !== "");
}

/**
 * Whether the Fork button may fire.
 *
 * A source clone is always required; then: `existing` needs a parseable ticket AND a preset
 * that claims its prefix — with the preset dropdown gone there is no way to override the
 * auto-selection, so a prefix nothing claims is a request the server would 400; `create` needs
 * a team key + title; `plain` and `template` a title + a preset whenever any are
 * configured.
 */
export function cloneDraftValid(
  draft: CloneDraft,
  {
    presets,
    preset,
    ticketParsed,
    keyMissing,
    needsSource = true,
  }: {
    presets: PresetRedacted[];
    preset: PresetRedacted | undefined;
    /** Whether `parseTicketInput` found an id in `draft.ticket`. */
    ticketParsed: boolean;
    keyMissing: boolean;
    /** False for template create, which picks an image instead of a source clone. */
    needsSource?: boolean;
  },
): boolean {
  const modeValid =
    draft.mode === "existing"
      ? ticketParsed && (presets.length === 0 || !!preset)
      : draft.mode === "create"
        ? draft.title.trim().length > 0 && draft.team.trim().length > 0
        : draft.mode === "template"
        ? draft.title.trim().length > 0 &&
          (presets.length === 0 || !!draft.templatePreset)
        : draft.title.trim().length > 0 && (presets.length === 0 || !!draft.plainPreset);
  return (!needsSource || !!draft.source) && modeValid && !keyMissing;
}

/** What the dialog should do about the clone operation it started. */
export type OpPhase = "running" | "done" | "failed";

/**
 * Classify the started operation from the live op list. This is the rule that decides when
 * the dialog closes, and it has one non-obvious case.
 *
 * Finished operations are PRUNED from `ControlState` shortly after they settle (8s after
 * Done, 60s after Error). A poll of the list can therefore miss the terminal frame entirely,
 * so **an op that has vanished after being seen counts as done** — the same rule the CLI's
 * waiter uses. `failed` is passed in as sticky state by the caller, because that vanish rule
 * would otherwise fire when a FAILED op is pruned and close the dialog over its own error.
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
