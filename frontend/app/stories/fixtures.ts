// Shared, API-free sample data for the Storybook stories. Nothing here touches the
// network — the components are all dependency-injected, so a story is just "this
// fixture + these callbacks". Kept in one place so the Sidebar / SidebarClone /
// Settings stories stay consistent.

import type { ClaudeUsage, Clone, Operation } from "~/lib/types";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { LxcStats } from "~/lib/wire/LxcStats";
import type { ImageInfo } from "~/lib/wire/ImageInfo";

const GiB = 1024 ** 3;

// --- clones (each covers a distinct visual state) ---------------------------

/** A managed clone actively working, pinned to one Claude account, on a ticket. */
export const cloneWorking: Clone = {
  id: "pega-we-142",
  host: "10.99.0.11",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  source: "pegasis0/rmng-template:latest",
  claudeAccountEmail: "alex@example.com",
  claudeSelection: "alex@example.com",
  linearWorkspace: "we",
  linearTicket: "WE-142",
  linearLabel: "frontend",
  displayName: "Normalize sidebar CPU to % of allowance",
  monitorState: "working",
};

/** Idle, balanced within the "pooled" Claude pool, with an unread dot (dropped from working). */
export const cloneIdle: Clone = {
  id: "pega-dev-88",
  host: "10.99.0.12",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  source: "pegasis0/rmng-template:latest",
  claudeAccountEmail: "sam@example.com",
  claudeGroup: "pooled",
  claudeSelection: "group:pooled",
  linearWorkspace: "dev",
  linearTicket: "DEV-88",
  displayName: "Wire up the pull-template wizard",
  monitorState: "idle",
  unread: true,
};

/** Offline (wrapper unreachable), Claude on auto — the server picked the account. */
export const cloneOffline: Clone = {
  id: "pega-hh-7",
  host: "10.99.0.13",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  claudeAccountEmail: "alex@example.com",
  claudeSelection: "auto",
  displayName: "Database migration spike",
  monitorState: "idle",
};

/** A managed scratch box deliberately left tokenless (no account installed). */
export const cloneNoToken: Clone = {
  id: "scratch-box",
  host: "10.99.0.20",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  claudeSelection: "none",
  monitorState: "idle",
};

/** A plain unmanaged row (no container) — only deletable, no commit/account actions.
 *  Carries no account fields at all: an unmanaged clone never binds one. */
export const cloneUnmanaged: Clone = {
  id: "legacy-desktop",
  host: "192.168.1.50",
  port: 3389,
  username: "admin",
  password: "",
  monitorState: "idle",
};

/** A managed clone holding BOTH providers: a pinned Claude account and a pooled Codex one.
 *  Exercises the two-line sidebar layout (one binding line per provider, CPU on the first /
 *  MEM on the second). */
export const cloneDualProvider: Clone = {
  id: "pega-dual-9",
  host: "10.99.0.14",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  source: "pegasis0/rmng-template:latest",
  claudeAccountEmail: "alex@example.com",
  claudeSelection: "alex@example.com",
  codexAccountEmail: "alex@openai.com",
  codexGroup: "team",
  codexSelection: "group:team",
  linearWorkspace: "we",
  linearTicket: "WE-207",
  displayName: "Port the encoder path to the new VA surface pool",
  monitorState: "working",
};

/** A sub clone: a managed clone spawned by `cloneWorking`, shown indented under it in the
 *  sidebar (collapsed by default). Cosmetic one-level nesting via `parent`. */
export const cloneSubClone: Clone = {
  id: "pega-we-142-helper",
  host: "10.99.0.31",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  source: "pegasis0/rmng-template:latest",
  claudeAccountEmail: "sam@example.com",
  claudeGroup: "pooled",
  claudeSelection: "group:pooled",
  parent: cloneWorking.id,
  displayName: "helper: run the e2e suite",
  monitorState: "working",
};

export const hosts: Clone[] = [
  cloneWorking,
  cloneSubClone,
  cloneIdle,
  cloneOffline,
  cloneNoToken,
  cloneUnmanaged,
  cloneDualProvider,
];
export const cloneIds: string[] = hosts.map((h) => h.id);

// --- live container usage (the volatile `stats` SSE map) --------------------

export const lxcStats: LxcStats = {
  cpuPct: 23,
  memUsed: BigInt(Math.round(18.7 * GiB)),
  memLimit: BigInt(264 * GiB),
  diskUsed: BigInt(Math.round(312.1 * GiB)),
};

export const stats: Record<string, ContainerStats> = {
  [cloneWorking.id]: {
    cpuPct: 40,
    memUsed: BigInt(Math.round(5.1 * GiB)),
    memLimit: BigInt(40 * GiB),
  },
  [cloneIdle.id]: {
    cpuPct: 1.2,
    memUsed: BigInt(Math.round(1.4 * GiB)),
    memLimit: BigInt(40 * GiB),
  },
  [cloneNoToken.id]: {
    cpuPct: 0.3,
    memUsed: BigInt(Math.round(0.6 * GiB)),
    memLimit: BigInt(40 * GiB),
  },
  [cloneDualProvider.id]: {
    cpuPct: 18,
    memUsed: BigInt(Math.round(3.2 * GiB)),
    memLimit: BigInt(40 * GiB),
  },
};

// --- per-account usage (display-only) --------------------------------------
// One flat list carrying BOTH providers' rows, tagged by `provider` — the shape of
// `ControlState.claudeAccounts`. Account ids are per-provider, not pool-scoped.

const usage = (
  a: Omit<ClaudeUsage, "id" | "active"> & Partial<Pick<ClaudeUsage, "active">>,
): ClaudeUsage => ({
  id: `${a.provider ?? "claude"}|${a.email}`,
  active: a.active ?? true,
  ...a,
});

export const claudeAccounts: ClaudeUsage[] = [
  usage({
    email: "alex@example.com",
    provider: "claude",
    assignable: true,
    lastUpdated: 1_700_000_000_000,
    fiveHour: { pct: 42, resetsAt: null },
    sevenDay: { pct: 61, resetsAt: null },
    fable: { pct: 8, resetsAt: null },
  }),
  usage({
    email: "sam@example.com",
    provider: "claude",
    active: false,
    assignable: true,
    lastUpdated: 1_700_000_000_000,
    fiveHour: { pct: 88, resetsAt: null },
    sevenDay: { pct: 73, resetsAt: null },
  }),
  usage({
    email: "alex@openai.com",
    provider: "codex",
    assignable: false,
    lastUpdated: 1_700_000_000_000,
    // Codex exposes only a weekly (7d) limit now — the 5h window was removed upstream.
    sevenDay: { pct: 40, resetsAt: null },
    resetCredits: 3n,
  }),
];

/** The configured account pools, per provider — the authoritative list for pickers. */
export const cloneGroups: CloneGroup[] = [
  { name: "pooled", accounts: ["alex@example.com", "sam@example.com"] },
  { name: "solo", accounts: ["alex@example.com"] },
];
export const codexGroups: CloneGroup[] = [{ name: "team", accounts: ["alex@openai.com"] }];

// --- clone-source images ----------------------------------------------------

export const images: ImageInfo[] = [
  {
    id: "sha256:aaaa0000",
    reference: "pegasis0/rmng-template:latest",
    sizeBytes: BigInt(6_800_000_000),
    createdAt: "2026-06-20T12:00:00Z",
    base: true,
    createdFrom: null,
    inUseBy: [cloneWorking.id, cloneIdle.id],
  },
  {
    id: "sha256:bbbb1111",
    reference: "node20:latest",
    sizeBytes: BigInt(7_200_000_000),
    createdAt: "2026-06-28T09:30:00Z",
    base: false,
    createdFrom: "pegasis0/rmng-template:latest",
    inUseBy: [],
  },
];

// --- operations -------------------------------------------------------------

/** A running clone op (drives the Activity list + disables the + Clone button). */
export const cloneOperation: Operation = {
  id: "op-clone-1",
  kind: "clone",
  target: "pega-per-9",
  source: "pegasis0/rmng-template:latest",
  status: "running",
  step: "provision",
  pct: 45,
  message: "Provisioning container…",
  log: ["pulling layers", "creating container", "starting gnome session"],
  startedAt: 1_700_000_000_000,
};

/** A running delete op targeting an existing clone (shows the row's busy state). */
export const deleteOperation: Operation = {
  id: "op-delete-1",
  kind: "delete",
  target: cloneIdle.id,
  status: "running",
  step: "stopping",
  pct: 30,
  message: "Stopping container…",
  log: ["stopping container"],
  startedAt: 1_700_000_000_000,
};

// --- redacted app config (for the Settings story) --------------------------

export const appConfig: AppConfigRedacted = {
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
  cloneGroups,
  codexGroups,
  presets: [
    {
      name: "webapp",
      labels: ["frontend", "webapp"],
      linearKeySet: true,
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
};

