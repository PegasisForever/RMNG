// The redacted config the Settings panel loads: same structure the server sends, with every
// secret already replaced by a boolean "is set".
//
// A builder and nothing else. The panel loads this into state and edits it, so a story that
// answered `getConfig` with one shared object would be editing every other story's config
// too. Call it per story, and per call inside a `getConfig` stub: a real fetch hands back a
// fresh parse each time, so a stub that does the same is also the honest one.

import { settingsDraftFrom, type SettingsDraft } from "~/lib/settingsDraft";
import { setupDraftFrom, type SetupDraft } from "~/lib/setupDraft";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { UpdateStatus } from "~/lib/wire/UpdateStatus";

import { makeCloneGroups, makeCodexGroups } from "./accounts";
import { makeClonePresets } from "./presets";

export function makeAppConfig(overrides: Partial<AppConfigRedacted> = {}): AppConfigRedacted {
  return {
    listen: { web: 9000, video: 9001, daemonMcp: 9004, forward: 9005, bastion: 2222 },
    agentPort: 4096,
    dataDir: "/data",
    staticDir: "",
    cloneSocket: "/srv/rmng-sock/clones.sock",
    setupComplete: true,
    layoutPresets: [
      {
        name: "Default",
        monitors: [
          { width: 2560, height: 1440, x: 0, y: 0, primary: true },
          { width: 1920, height: 1080, x: 2560, y: 0, primary: false },
        ],
      },
    ],
    activeLayout: "Default",
    docker: {
      socket: "/var/run/docker.sock",
      subnet: "10.99.0.0/24",
      hostnamePrefix: "pega-",
      cloneCpus: 16,
      cloneMemoryMb: 32768,
      templateReference: "pegasis0/rmng-template:latest",
      serverImage: "pegasis0/rmng:latest",
      buildInfraEnabled: true,
      registryImage: "registry:2.8.3",
      buildkitImage: "moby/buildkit:v0.17.2",
      buildkitCacheGb: 40,
    },
    claude: {
      pollSecs: BigInt(600),
      pinnedEmail: "alex@example.com",
    },
    codex: { pollSecs: BigInt(600), pinnedEmail: null, usagePolling: true, autoReset: false },
    // Built per call, down to the member list, so two configs from this builder never share
    // an array with each other or with the accounts fixture module.
    cloneGroups: makeCloneGroups(),
    codexGroups: makeCodexGroups(),
    // The same set both dialogs resolve against: the config IS where their team keys come
    // from, so the Settings panel lists exactly the presets the ticket dialog offers.
    presets: makeClonePresets(),
    chroma: "yuv420",
    ssh: {
      authorizedKeys: ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeStorybookDemoKeyOnly alex@laptop"],
      publicHost: "rmng.example.com",
    },
    agentPlaybook: "# Desktop agent — operating notes\n\n(sample playbook)\n",
    globalPrompt: "# Working in this clone\n\n(sample shared operating memory)\n",
    ...overrides,
  };
}

/** The settings form, seeded from the config above through the same function the container
 *  uses. Deriving it rather than hand-writing it is what stops a story from showing a form
 *  the panel could never actually be in.
 *
 *  Freshly built, arrays and all: the sections replace what they are given, but every story
 *  that edits one would otherwise be editing the next story's form too. */
export function makeSettingsDraft(overrides: Partial<SettingsDraft> = {}): SettingsDraft {
  return { ...settingsDraftFrom(makeAppConfig()), ...overrides };
}

/** The first-run wizard's form, seeded from the config above through the same function the
 *  wizard's container uses.
 *
 *  Freshly built, monitors and ports included: the Server step replaces what it is given, but
 *  every story that drags a monitor would otherwise be dragging the next story's too. */
export function makeSetupDraft(overrides: Partial<SetupDraft> = {}): SetupDraft {
  return { ...setupDraftFrom(makeAppConfig({ setupComplete: false })), ...overrides };
}

/** The control-server's own version, as `GET /api/server/version` answers it. Up to date by
 *  default; override `available` for the state that lights the Update button. */
export function makeUpdateStatus(overrides: Partial<UpdateStatus> = {}): UpdateStatus {
  const digest = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
  return {
    currentRevision: "a1b2c3d",
    currentCreated: "2026-07-01T12:00:00Z",
    currentDigest: digest,
    remoteDigest: digest,
    available: false,
    reference: "pegasis0/rmng:latest",
    error: null,
    ...overrides,
  };
}
