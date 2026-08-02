// Clone presets, redacted the way the browser gets them: the Linear API key is a boolean
// rather than a key.
//
// A preset's labels ARE the team keys the ticket and clone dialogs offer, so what a preset
// resolves to decides three things on screen: which team keys the New-ticket tab and the
// ticket dialog list, whether the request needs a Linear key the config does not have, and
// what each account picker's blank option says the default is. So the set below is built to
// cover those, not to look like a real deployment.

import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

export function makePreset(overrides: Partial<PresetRedacted> = {}): PresetRedacted {
  return {
    name: "webapp",
    labels: ["WE", "frontend"],
    linearKey: "lin_api_fixture",
    claudeAccount: "",
    codexAccount: "",
    vars: [],
    agentPlaybook: "",
    globalPrompt: "",
    ...overrides,
  };
}

/** The presets both dialogs resolve against, freshly built.
 *
 *  The team keys match the ticket fixtures, WE and DEV, so a ticket in the column and a
 *  ticket the dialog opens land in the same teams. Between them: two team keys that map to
 *  different presets (so the team dropdown has a real choice), one preset that defaults its
 *  clones to a Claude pool and one that defaults to nothing (so both of the CLAUDE picker's
 *  blank labels are reachable), and `platform`, the one with no Linear key, so the
 *  key-missing warning is reachable by picking OPS.
 *
 *  No preset here names a Codex default, so the Codex picker's blank option reads
 *  "Preset default / auto" against every one of them. That is a real deployment: a Codex
 *  default is optional and most presets do without. A story that needs the other Codex label
 *  builds its own preset with `makePreset` rather than adding a default here, because these
 *  three also drive the ticket dialog's team dropdown. */
export function makeClonePresets(): PresetRedacted[] {
  return [
    makePreset({
      name: "webapp",
      labels: ["WE", "frontend"],
      linearKey: "lin_api_fixture",
      // A preset that defaults its clones to a pool; Codex left with no default.
      claudeAccount: "group:pooled",
      vars: [{ key: "NODE_ENV", value: "development" }],
    }),
    makePreset({
      name: "devtools",
      labels: ["DEV"],
      linearKey: "lin_api_fixture",
    }),
    makePreset({
      name: "platform",
      labels: ["OPS"],
      linearKey: "",
    }),
  ];
}
