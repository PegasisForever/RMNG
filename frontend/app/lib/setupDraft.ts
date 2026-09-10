// The first-run wizard's model, and every rule that reads or writes it. No React, no network:
// the container holds a `SetupDraft` in state and the View renders one, so both sides agree on
// what "the wizard" is, and a story can build one with `makeSetupDraft`.
//
// The wizard is NOT a small settings panel, and the difference is the whole reason this module
// exists next to `settingsDraft.ts` rather than inside it:
//
//   - The panel saves the entire config in one PUT. The wizard saves ONE STEP AT A TIME, and
//     each step's patch names only the fields that step edits. `merge_update` merges what it
//     is given, so a partial patch leaves the rest of the config alone.
//   - The panel rewrites `layoutPresets` wholesale from the form, clamping and trimming every
//     preset in the list. The wizard edits ONE arrangement (the active preset, else the first)
//     and round-trips the others exactly as the server sent them. `settingsPatch` would
//     rewrite presets the wizard never showed the operator.

// Only `monitorPatch` is genuinely the same rule, so only `monitorPatch` is shared.

import { monitorPatch, type MonitorDraft } from "~/lib/settingsDraft";
import type { Operation } from "~/lib/types";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ChromaMode } from "~/lib/wire/ChromaMode";

/** The wizard's steps, in order. The indexes are the step numbers everywhere else. */
export const SETUP_STEPS = ["Environment", "Server", "Finish"] as const;

/** Everything the first-run wizard can edit, as one model. */
export interface SetupDraft {
 hostnamePrefix: string;
 cloneCpus: number;
 cloneMemoryMb: number;
 /** The one monitor arrangement the wizard edits. Which named preset it belongs to comes
  *  from the config, not from here — the wizard has no preset picker. */
 monitors: MonitorDraft[];
 chroma: ChromaMode;
}

/** The name of the arrangement the wizard edits, mirroring the server's
 *  `effective_monitors()`: the active preset, else the first, else "Default". */
export function activeLayoutName(c: AppConfigRedacted): string {
 const active =
  c.layoutPresets.find((p) => p.name === c.activeLayout) ?? c.layoutPresets[0];
 return active?.name || "Default";
}

/** Seed the wizard from the server's redacted config.
 *
 *  The monitors are copied down to each member: the editor replaces rather than mutates, but
 *  sharing the server payload's array would leave the loaded config and the form aliased, and
 *  the step-2 patch reads BOTH (the edited arrangement from the form, the other presets from
 *  the config). A rig with no preset at all gets a single 1080p one to edit rather than an
 *  empty list, which is the same call `settingsDraftFrom` makes for the panel. */
export function setupDraftFrom(c: AppConfigRedacted): SetupDraft {
 const active =
  c.layoutPresets.find((p) => p.name === c.activeLayout) ?? c.layoutPresets[0];
 return {
  hostnamePrefix: c.docker.hostnamePrefix,
  cloneCpus: c.docker.cloneCpus,
  cloneMemoryMb: c.docker.cloneMemoryMb,
  monitors: active?.monitors.length
   ? active.monitors.map((m) => ({ ...m }))
   : [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }],
  chroma: c.chroma,
 };
}

/**
 * The whole `layoutPresets` array as step 2 sends it.
 *
 * Round-trip every existing preset instead of sending just the one being edited. The server's
 * `merge_update` replaces the whole `layoutPresets` array wholesale (that is how a delete is
 * expressed), so a single-element patch here would silently drop every other named preset on a
 * mature config.
 *
 * The presets the wizard did not show go back exactly as they arrived. Only the edited
 * arrangement is clamped, because only it came out of a number input.
 */
export function layoutPresetsPatch(
 draft: SetupDraft,
 config: AppConfigRedacted,
) {
 const name = activeLayoutName(config);
 const monitors = draft.monitors.map(monitorPatch);
 const existing = config.layoutPresets;
 if (!existing.length) {
  return [{ name: name || "Default", monitors }];
 }
 const updated = existing.map((p) =>
  p.name === name ? { ...p, monitors } : p,
 );
 return existing.some((p) => p.name === name)
  ? updated
  : [...updated, { name, monitors }];
}

/**
 * What step 2 (Server) sends: the fleet defaults, the edited arrangement, and the ports.
 *
 * `docker` names only the three fields this step edits.
 */
export function serverPatch(
 draft: SetupDraft,
 config: AppConfigRedacted,
): unknown {
 return {
  docker: {
   hostnamePrefix: draft.hostnamePrefix,
   cloneCpus: draft.cloneCpus,
   cloneMemoryMb: draft.cloneMemoryMb,
  },
  layoutPresets: layoutPresetsPatch(draft, config),
  chroma: draft.chroma,
 };
}

/** Whether the wizard refuses to advance.
 *
 *  The Environment step blocks until every required check passes. Every step blocks while
 *  a save is in flight. (The clone-network subnet used to gate this step; it is hardcoded
 *  on the server now.) */
export function nextDisabled(args: {
 step: number;
 saving: boolean;
 /** Every required environment check passes. */
 envOk: boolean;
}): boolean {
 return args.saving || (args.step === 0 && !args.envOk);
}
