// The redacted config the Settings panel loads: same structure the server sends, with every
// secret already replaced by a boolean "is set".

import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";

import { cloneGroups, codexGroups } from "./accounts";

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
    // Copied per call, down to the member list, so two configs built from this builder never
    // share an array with each other or with the accounts fixture module.
    cloneGroups: cloneGroups.map((g) => ({ ...g, accounts: [...g.accounts] })),
    codexGroups: codexGroups.map((g) => ({ ...g, accounts: [...g.accounts] })),
    presets: [
      {
        name: "webapp",
        labels: ["frontend", "webapp"],
        linearKeySet: true,
        // A preset that defaults its clones to a pool; Codex left with no default.
        claudeAccount: "group:pooled",
        codexAccount: "",
        vars: [{ key: "NODE_ENV", value: "development" }],
        agentPlaybook: "",
        globalPrompt: "",
      },
    ],
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

export const appConfig: AppConfigRedacted = makeAppConfig();
