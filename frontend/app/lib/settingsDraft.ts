// The settings form's model, and every rule that reads or writes it. No React, no network:
// the container holds a `SettingsDraft` in state and the View renders one, so both sides
// agree on what "the form" is, and a story can build one with `makeSettingsDraft`.
//
// Three rules live here because they are the ones a many-way component split endangers, and
// none of them belongs to any single section:
//
//   - `settingsDraftFrom` seeds the form from the server's redacted config, including the
//     one place a blank config becomes a visible default (a rig with no layout preset gets a
//     1080p one to edit rather than an empty list).
//   - `settingsPatch` decides what a save actually sends: what is trimmed, what is dropped
//     for being half-typed, what is deduped, and the one field that is only sent before
//     first-run setup finishes.
//   - `orderedAccounts` applies the operator's own cosmetic ordering to the two account
//     lists.

import { ordered, type AcctOrder } from "~/lib/accountOrder";
import type { ClaudeUsage } from "~/lib/types";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ChromaMode } from "~/lib/wire/ChromaMode";
import type { ListenConfig } from "~/lib/wire/ListenConfig";
import type { SshConfig } from "~/lib/wire/SshConfig";

/** One monitor in a layout preset. Same shape as the wire `MonitorSpec`; named separately
 *  because the editor writes it in place. */
export interface MonitorDraft {
  width: number;
  height: number;
  x: number;
  y: number;
  primary: boolean;
}

/** A named monitor arrangement, as the form edits it. */
export interface LayoutPresetDraft {
  name: string;
  monitors: MonitorDraft[];
}

/** A named account pool: a name plus the member emails ticked in its checkbox grid. */
export interface GroupDraft {
  name: string;
  accounts: string[];
}

/** A clone preset, as the form edits it.
 *
 *  Two fields differ from the wire shape. `labels` is one comma-separated string because that
 *  is what the operator types, and it is split back apart on save. `linearKey` is write-only:
 *  it starts blank meaning "keep whatever the server holds", and `keySet` carries whether the
 *  server holds anything, which is what the set/unset badge reads. */
export interface PresetDraft {
  name: string;
  labels: string;
  linearKey: string;
  keySet: boolean;
  claudeAccount: string;
  codexAccount: string;
  vars: { key: string; value: string }[];
  agentPlaybook: string;
  globalPrompt: string;
}

/** Everything the settings form can edit, as one model. */
export interface SettingsDraft {
  layoutPresets: LayoutPresetDraft[];
  presets: PresetDraft[];
  hostnamePrefix: string;
  templateReference: string;
  /** One-time: baked into the rmng bridge and every clone IP at first-run setup. */
  subnet: string;
  cloneCpus: number;
  cloneMemoryMb: number;
  claude: { pollSecs: number; pinnedEmail: string };
  claudeGroups: GroupDraft[];
  codex: { pollSecs: number; pinnedEmail: string; usagePolling: boolean; autoReset: boolean };
  codexGroups: GroupDraft[];
  listen: ListenConfig;
  agentPort: number;
  dataDir: string;
  staticDir: string;
  cloneSocket: string;
  chroma: ChromaMode;
  agentPlaybook: string;
  globalPrompt: string;
  ssh: SshConfig;
  /** Write-only, like every preset's Linear key: blank means "keep the stored one". Unlike
   *  those, the stored value never comes back — the browser has no use for it. */
  openrouterKey: string;
  /** Whether the server holds one. The only thing the panel can know about it. */
  openrouterKeySet: boolean;
  openrouterModel: string;
}

/** The layout preset a rig with none configured is given to edit. Offering an empty list
 *  would leave the operator with nothing to type into. */
export function newLayoutPreset(name = ""): LayoutPresetDraft {
  return { name, monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }] };
}

/** A blank preset row. Both account defaults start empty: a new preset takes no opinion on
 *  which pool its clones get until the operator gives it one. */
export function newPreset(): PresetDraft {
  return {
    name: "",
    labels: "",
    linearKey: "",
    keySet: false,
    claudeAccount: "",
    codexAccount: "",
    vars: [{ key: "", value: "" }],
    agentPlaybook: "",
    globalPrompt: "",
  };
}

/** A blank pool row. */
export function newGroup(): GroupDraft {
  return { name: "", accounts: [] };
}

/** One monitor as a save sends it: a size of at least 1, an offset of at least 0.
 *
 *  The number inputs accept anything a keyboard can produce, including a blank field that
 *  reads back as 0, and a monitor 0 pixels wide is not a monitor. Shared with the setup
 *  wizard, which clamps the arrangement it edits by exactly this rule. */
export function monitorPatch(m: MonitorDraft): MonitorDraft {
  return {
    width: Math.max(1, m.width),
    height: Math.max(1, m.height),
    x: Math.max(0, m.x),
    y: Math.max(0, m.y),
    primary: m.primary,
  };
}

/** Seed the form from the server's redacted config.
 *
 *  Every array is copied down to its members. The editors below replace rather than mutate,
 *  but sharing the server payload's arrays would leave the loaded config and the form
 *  aliased, and a re-seed after save would then compare a value against itself. */
export function settingsDraftFrom(c: AppConfigRedacted): SettingsDraft {
  return {
    layoutPresets: c.layoutPresets.length
      ? c.layoutPresets.map((p) => ({ name: p.name, monitors: p.monitors.map((m) => ({ ...m })) }))
      : [newLayoutPreset("Default")],
    presets: c.presets.map((p) => ({
      name: p.name,
      labels: p.labels.join(", "),
      linearKey: "",
      // The server vends the key itself now. The input stays write-only all the same: it
      // exists to replace a key, not to read one back, and `""` still means "keep stored".
      keySet: p.linearKey !== "",
      claudeAccount: p.claudeAccount,
      codexAccount: p.codexAccount,
      vars: p.vars.map((v) => ({ ...v })),
      agentPlaybook: p.agentPlaybook,
      globalPrompt: p.globalPrompt,
    })),
    hostnamePrefix: c.docker.hostnamePrefix,
    templateReference: c.docker.templateReference,
    subnet: c.docker.subnet,
    cloneCpus: c.docker.cloneCpus,
    cloneMemoryMb: c.docker.cloneMemoryMb,
    claude: {
      ...c.claude,
      pollSecs: Number(c.claude.pollSecs),
      pinnedEmail: c.claude.pinnedEmail ?? "",
    },
    claudeGroups: c.cloneGroups.map((g) => ({ name: g.name, accounts: [...g.accounts] })),
    codex: {
      ...c.codex,
      pollSecs: Number(c.codex.pollSecs),
      pinnedEmail: c.codex.pinnedEmail ?? "",
    },
    codexGroups: c.codexGroups.map((g) => ({ name: g.name, accounts: [...g.accounts] })),
    listen: { ...c.listen },
    agentPort: c.agentPort,
    dataDir: c.dataDir,
    staticDir: c.staticDir,
    cloneSocket: c.cloneSocket,
    chroma: c.chroma,
    agentPlaybook: c.agentPlaybook,
    globalPrompt: c.globalPrompt,
    // Seeded blank on purpose: the input is write-only, and re-seeding from the server's
    // response after a save is what clears it.
    openrouterKey: "",
    openrouterKeySet: c.openrouterKeySet,
    openrouterModel: c.openrouterModel,
    ssh: {
      authorizedKeys: c.ssh?.authorizedKeys ?? [],
      publicHost: c.ssh?.publicHost ?? "",
    },
  };
}

/** Half-typed rows are dropped rather than saved as unnamed pools, and members are deduped —
 *  the checkbox editor cannot produce a duplicate, but a hand-edited config can, and a
 *  repeated email would skew group selection. */
function savedGroups(groups: GroupDraft[]): GroupDraft[] {
  return groups
    .filter((g) => g.name.trim())
    .map((g) => ({ name: g.name.trim(), accounts: [...new Set(g.accounts)] }));
}

/**
 * What a save sends.
 *
 * `setupComplete` is the config's own flag, and it gates exactly one field: the subnet is
 * baked into the rmng bridge and every clone's static IP at first-run setup, so after setup
 * it is read-only and the server rejects a change anyway. Before setup it is sent; after, it
 * is omitted entirely rather than sent unchanged.
 */
export function settingsPatch(draft: SettingsDraft, setupComplete: boolean): unknown {
  return {
    layoutPresets: draft.layoutPresets
      .filter((p) => p.name.trim())
      .map((p) => ({
        name: p.name.trim(),
        monitors: p.monitors.map(monitorPatch),
      })),
    docker: {
      hostnamePrefix: draft.hostnamePrefix,
      templateReference: draft.templateReference,
      cloneCpus: draft.cloneCpus,
      cloneMemoryMb: draft.cloneMemoryMb,
      ...(setupComplete ? {} : { subnet: draft.subnet }),
    },
    claude: { ...draft.claude, pinnedEmail: draft.claude.pinnedEmail || null },
    cloneGroups: savedGroups(draft.claudeGroups),
    codex: { ...draft.codex, pinnedEmail: draft.codex.pinnedEmail || null },
    codexGroups: savedGroups(draft.codexGroups),
    listen: draft.listen,
    agentPort: draft.agentPort,
    dataDir: draft.dataDir,
    staticDir: draft.staticDir,
    cloneSocket: draft.cloneSocket,
    chroma: draft.chroma,
    ssh: draft.ssh,
    agentPlaybook: draft.agentPlaybook,
    globalPrompt: draft.globalPrompt,
    openrouter: {
      key: draft.openrouterKey, // "" = keep the stored key
      model: draft.openrouterModel,
    },
    presets: draft.presets
      .filter((p) => p.name.trim())
      .map((p) => ({
        name: p.name.trim(),
        labels: p.labels.split(",").map((s) => s.trim()).filter(Boolean),
        linearKey: p.linearKey, // "" = keep the stored key
        // Unlike linearKey a blank here is MEANINGFUL ("no default — let the clone decide"),
        // so it is sent as-is rather than treated as "keep stored".
        claudeAccount: p.claudeAccount,
        codexAccount: p.codexAccount,
        vars: p.vars.filter((v) => v.key.trim()).map((v) => ({ key: v.key.trim(), value: v.value })),
        agentPlaybook: p.agentPlaybook,
        globalPrompt: p.globalPrompt,
      })),
  };
}

/**
 * Split the flat both-provider account list into the two lists the panel draws, each in the
 * operator's own saved order.
 *
 * `provider` was added to the row after the fact, so a row that predates it (absent or null)
 * is Claude — anything else has to be tagged explicitly.
 */
export function orderedAccounts(
  accounts: ClaudeUsage[],
  order: AcctOrder,
): { claude: ClaudeUsage[]; codex: ClaudeUsage[] } {
  return {
    claude: ordered(
      accounts.filter((a) => (a.provider ?? "claude") === "claude"),
      order.claude ?? [],
      (a) => a.id,
    ),
    codex: ordered(
      accounts.filter((a) => a.provider === "codex"),
      order.codex ?? [],
      (a) => a.id,
    ),
  };
}
