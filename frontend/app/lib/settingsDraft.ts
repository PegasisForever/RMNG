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
//
// (Cosmetic account ordering used to be a third rule here; it moved to `accountOrder`,
// alongside the store and the sort it belongs with.)

import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ChromaMode } from "~/lib/wire/ChromaMode";
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
  claudeAccount: string;
  codexAccount: string;
  agentPlaybook: string;
  globalPrompt: string;
  startupScript: string;
  dockerfile: string;
}

/** Everything the settings form can edit, as one model. */
export interface SettingsDraft {
  layoutPresets: LayoutPresetDraft[];
  presets: PresetDraft[];
  hostnamePrefix: string;
  cloneCpus: number;
  cloneMemoryMb: number;
  /** The single pool list: each pool may mix Claude and Codex accounts. */
  groups: GroupDraft[];
  codex: { autoReset: boolean };
  chroma: ChromaMode;
  agentPlaybook: string;
  globalPrompt: string;
  ssh: SshConfig;
  /** Which GPT answers the stuck question, and which Codex account pays for it.
   *  `codexEmail` is flattened to "" here so no input has to handle a null. */
  judge: { codexModel: string; codexEmail: string };
}

/** The layout preset a rig with none configured is given to edit. Offering an empty list
 *  would leave the operator with nothing to type into. */
export function newLayoutPreset(name = ""): LayoutPresetDraft {
  return {
    name,
    monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }],
  };
}

/** A blank preset row. Both account defaults start empty: a new preset takes no opinion on
 *  which pool its clones get until the operator gives it one. */
export function newPreset(): PresetDraft {
  return {
    name: "",
    labels: "",
    linearKey: "",
    claudeAccount: "",
    codexAccount: "",
    agentPlaybook: "",
    globalPrompt: "",
    startupScript: "",
    dockerfile: "FROM pegasis0/rmng-template:latest",
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
      ? c.layoutPresets.map((p) => ({
          name: p.name,
          monitors: p.monitors.map((m) => ({ ...m })),
        }))
      : [newLayoutPreset("Default")],
    presets: c.presets.map((p) => ({
      name: p.name,
      labels: p.labels.join(", "),
      linearKey: p.linearKey,
      claudeAccount: p.claudeAccount,
      codexAccount: p.codexAccount,
      agentPlaybook: p.agentPlaybook,
      globalPrompt: p.globalPrompt,
      startupScript: p.startupScript ?? "",
      dockerfile: p.dockerfile ?? "FROM pegasis0/rmng-template:latest",
    })),
    hostnamePrefix: c.docker.hostnamePrefix,
    cloneCpus: c.docker.cloneCpus,
    cloneMemoryMb: c.docker.cloneMemoryMb,
    groups: c.groups.map((g) => ({
      name: g.name,
      accounts: [...g.accounts],
    })),
    codex: {
      autoReset: c.codex.autoReset,
    },
    chroma: c.chroma,
    agentPlaybook: c.agentPlaybook,
    globalPrompt: c.globalPrompt,
    judge: {
      codexModel: c.judge?.codexModel ?? "",
      codexEmail: c.judge?.codexEmail ?? "",
    },
    ssh: {
      authorizedKeys: c.ssh?.authorizedKeys ?? [],
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
 * What a save sends. (`setupComplete` is kept as a parameter because the wizard shares the
 * save path; no field in the patch is gated on it anymore — the one-time subnet is
 * hardcoded.)
 */
export function settingsPatch(
  draft: SettingsDraft,
  _setupComplete: boolean,
): unknown {
  return {
    layoutPresets: draft.layoutPresets
      .filter((p) => p.name.trim())
      .map((p) => ({
        name: p.name.trim(),
        monitors: p.monitors.map(monitorPatch),
      })),
    docker: {
      hostnamePrefix: draft.hostnamePrefix,
      cloneCpus: draft.cloneCpus,
      cloneMemoryMb: draft.cloneMemoryMb,
    },
    groups: savedGroups(draft.groups),
    codex: { autoReset: draft.codex.autoReset },
    chroma: draft.chroma,
    ssh: draft.ssh,
    agentPlaybook: draft.agentPlaybook,
    globalPrompt: draft.globalPrompt,
    // `null` rather than "", which the server reads as "keep stored": picking an account and
    // then going back to "the first one" has to be sendable.
    judge: { ...draft.judge, codexEmail: draft.judge.codexEmail || null },
    presets: draft.presets
      .filter((p) => p.name.trim())
      .map((p) => ({
        name: p.name.trim(),
        labels: p.labels
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
        linearKey: p.linearKey,
        // Unlike linearKey a blank here is MEANINGFUL ("no default — let the clone decide"),
        // so it is sent as-is rather than treated as "keep stored".
        claudeAccount: p.claudeAccount,
        codexAccount: p.codexAccount,
        agentPlaybook: p.agentPlaybook,
        globalPrompt: p.globalPrompt,
        startupScript: p.startupScript,
        dockerfile:
          p.dockerfile.trim() === ""
            ? "FROM pegasis0/rmng-template:latest"
            : p.dockerfile,
      })),
  };
}
