// The first-run wizard's model, and every rule that reads or writes it. No React, no network:
// the container holds a `SetupDraft` in state and the View renders one, so both sides agree on
// what "the wizard" is, and a story can build one with `makeSetupDraft`.
//
// The wizard is NOT a small settings panel, which is why this module exists next to
// `settingsDraft.ts` rather than inside it: the panel saves the entire config in one PUT, the
// wizard saves ONE STEP AT A TIME, and each step's patch names only the fields that step edits.
// `merge_update` merges what it is given, so a partial patch leaves the rest of the config alone.

import type { Operation } from "~/lib/types";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";

/** The wizard's steps, in order. The indexes are the step numbers everywhere else. */
export const SETUP_STEPS = ["Environment", "Server", "Finish"] as const;

/** Everything the first-run wizard can edit, as one model. */
export interface SetupDraft {
 hostnamePrefix: string;
 cloneCpus: number;
 cloneMemoryMb: number;
}

/** Seed the wizard from the server's redacted config. */
export function setupDraftFrom(c: AppConfigRedacted): SetupDraft {
 return {
  hostnamePrefix: c.docker.hostnamePrefix,
  cloneCpus: c.docker.cloneCpus,
  cloneMemoryMb: c.docker.cloneMemoryMb,
 };
}

/**
 * What step 2 (Server) sends: the fleet defaults.
 *
 * `docker` names only the three fields this step edits.
 */
export function serverPatch(draft: SetupDraft): unknown {
 return {
  docker: {
   hostnamePrefix: draft.hostnamePrefix,
   cloneCpus: draft.cloneCpus,
   cloneMemoryMb: draft.cloneMemoryMb,
  },
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
