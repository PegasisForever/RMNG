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
//   - The wizard never saves `templateReference`. Step 3 pulls that reference; the field is
//     the pull's argument, not a setting the wizard persists.
//
// Only `monitorPatch` is genuinely the same rule, so only `monitorPatch` is shared.

import { monitorPatch, type MonitorDraft } from "~/lib/settingsDraft";
import type { Operation } from "~/lib/types";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ChromaMode } from "~/lib/wire/ChromaMode";
import type { ListenConfig } from "~/lib/wire/ListenConfig";

/** The wizard's steps, in order. The indexes are the step numbers everywhere else. */
export const SETUP_STEPS = ["Environment", "Server", "Download template", "Finish"] as const;

/** Everything the first-run wizard can edit, as one model. */
export interface SetupDraft {
  /** One-time: baked into the rmng bridge and every clone's static IP when setup latches. */
  subnet: string;
  hostnamePrefix: string;
  cloneCpus: number;
  cloneMemoryMb: number;
  /** The one monitor arrangement the wizard edits. Which named preset it belongs to comes
   *  from the config, not from here — the wizard has no preset picker. */
  monitors: MonitorDraft[];
  chroma: ChromaMode;
  listen: ListenConfig;
  agentPort: number;
  /** The reference step 3 pulls. Never persisted: it is the pull's argument. */
  templateReference: string;
}

/** Mirror of the server's `validate_docker_subnet`: an IPv4 CIDR with a /16–/24 prefix. */
export function isValidSubnet(s: string): boolean {
  const [ip, prefix, ...rest] = s.split("/");
  if (rest.length > 0 || prefix === undefined) return false;
  const p = Number(prefix);
  if (!Number.isInteger(p) || p < 16 || p > 24) return false;
  const octets = ip.split(".");
  return (
    octets.length === 4 &&
    octets.every((o) => /^\d+$/.test(o) && Number(o) >= 0 && Number(o) <= 255)
  );
}

/** The subnet field holds something the server will accept. Blank is not valid: the wizard
 *  cannot create the bridge without one. */
export function subnetOk(subnet: string): boolean {
  return subnet.trim().length > 0 && isValidSubnet(subnet.trim());
}

/** The name of the arrangement the wizard edits, mirroring the server's
 *  `effective_monitors()`: the active preset, else the first, else "Default". */
export function activeLayoutName(c: AppConfigRedacted): string {
  const active = c.layoutPresets.find((p) => p.name === c.activeLayout) ?? c.layoutPresets[0];
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
  const active = c.layoutPresets.find((p) => p.name === c.activeLayout) ?? c.layoutPresets[0];
  return {
    subnet: c.docker.subnet,
    hostnamePrefix: c.docker.hostnamePrefix,
    cloneCpus: c.docker.cloneCpus,
    cloneMemoryMb: c.docker.cloneMemoryMb,
    monitors: active?.monitors.length
      ? active.monitors.map((m) => ({ ...m }))
      : [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }],
    chroma: c.chroma,
    listen: { ...c.listen },
    agentPort: c.agentPort,
    templateReference: c.docker.templateReference,
  };
}

/**
 * What step 1 (Environment) sends: the one-time subnet, trimmed.
 *
 * The server latches it when the Finish step flips `setupComplete`, so this is the last
 * moment it can be written.
 */
export function subnetPatch(draft: SetupDraft): unknown {
  return { docker: { subnet: draft.subnet.trim() } };
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
export function layoutPresetsPatch(draft: SetupDraft, config: AppConfigRedacted) {
  const name = activeLayoutName(config);
  const monitors = draft.monitors.map(monitorPatch);
  const existing = config.layoutPresets;
  if (!existing.length) {
    return [{ name: name || "Default", monitors }];
  }
  const updated = existing.map((p) => (p.name === name ? { ...p, monitors } : p));
  return existing.some((p) => p.name === name) ? updated : [...updated, { name, monitors }];
}

/**
 * What step 2 (Server) sends: the fleet defaults, the edited arrangement, and the ports.
 *
 * `docker` names only the three fields this step edits. The subnet went in step 1 and the
 * template reference is never persisted, so neither appears here.
 */
export function serverPatch(draft: SetupDraft, config: AppConfigRedacted): unknown {
  return {
    docker: {
      hostnamePrefix: draft.hostnamePrefix,
      cloneCpus: draft.cloneCpus,
      cloneMemoryMb: draft.cloneMemoryMb,
    },
    layoutPresets: layoutPresetsPatch(draft, config),
    chroma: draft.chroma,
    listen: draft.listen,
    agentPort: draft.agentPort,
  };
}

/** The exact reference the server will pull: what was typed, else the configured default.
 *  Resolved here so the started op's `target` is a value the wizard already holds. */
export function pullReference(draft: SetupDraft, config: AppConfigRedacted): string {
  return draft.templateReference.trim() || config.docker.templateReference;
}

/** The pull op is kind "pull" with target === the pulled reference (jobs.rs `start_pull` →
 *  `make_op(Pull, reference, None)`). */
export function findPullOperation(
  operations: Operation[],
  target: string | null,
): Operation | undefined {
  return target ? operations.find((o) => o.kind === "pull" && o.target === target) : undefined;
}

/** The Download button may fire: something to pull, and no pull already under way or done. */
export function canPull(args: {
  templateReference: string;
  /** The POST that starts the pull is in flight. */
  pulling: boolean;
  pullRunning: boolean;
  pullDone: boolean;
}): boolean {
  return (
    args.templateReference.trim().length > 0 &&
    !args.pulling &&
    !args.pullRunning &&
    !args.pullDone
  );
}

/** Whether the wizard refuses to advance.
 *
 *  The Environment step blocks until every required check passes AND the subnet is valid; the
 *  Download-template step blocks while a pull is running (leaving mid-pull would strand it).
 *  Every step blocks while a save is in flight. */
export function nextDisabled(args: {
  step: number;
  saving: boolean;
  /** Every required environment check passes. */
  envOk: boolean;
  subnetOk: boolean;
  pullRunning: boolean;
}): boolean {
  return (
    args.saving ||
    (args.step === 0 && (!args.envOk || !args.subnetOk)) ||
    (args.step === 2 && args.pullRunning)
  );
}
