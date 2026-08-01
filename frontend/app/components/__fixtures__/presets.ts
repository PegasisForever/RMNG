// Clone presets, redacted the way the browser gets them: the Linear API key is a boolean
// rather than a key.
//
// A preset is what the clone dialog resolves to, and what it resolves to decides three things
// on screen: which team keys the New-ticket tab offers, whether the request needs a Linear key
// the config does not have, and what each account picker's blank option says the default is.
// So the set below is built to cover those, not to look like a real deployment.

import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

export function makePreset(overrides: Partial<PresetRedacted> = {}): PresetRedacted {
  return {
    name: "webapp",
    labels: ["frontend", "webapp"],
    linearKeySet: true,
    claudeAccount: "",
    codexAccount: "",
    vars: [],
    agentPlaybook: "",
    globalPrompt: "",
    ...overrides,
  };
}

/** The presets the clone dialog resolves against, freshly built.
 *
 *  Between them: two team keys that map to different presets (so the New-ticket dropdown has
 *  a real choice), one preset that defaults its clones to a pool and one that defaults to
 *  nothing (so both blank labels are reachable), and one preset with no Linear key (so the
 *  key-missing warning is reachable by picking its team). */
export function makeClonePresets(): PresetRedacted[] {
  return [
    makePreset({
      name: "webapp",
      labels: ["WE", "DEV"],
      linearKeySet: true,
      claudeAccount: "group:pooled",
      codexAccount: "alex@openai.com",
    }),
    makePreset({
      name: "spike",
      labels: ["PER"],
      linearKeySet: false,
      claudeAccount: "",
      codexAccount: "",
    }),
  ];
}
