// Shared, API-free sample data for the Storybook stories. Nothing here touches the
// network — the components are all dependency-injected, so a story is just "this
// fixture + these callbacks". Kept in one place so the Sidebar / SidebarHost /
// Settings stories stay consistent.

import type { ClaudeUsage, GroupUsage, Host, Operation } from "~/lib/types";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { Group } from "~/lib/wire/Group";
import type { ImageInfo } from "~/lib/wire/ImageInfo";

const GiB = 1024 ** 3;

// --- hosts (each covers a distinct visual state) ---------------------------

/** A managed clone actively working, bound to the "pooled" account group, on a ticket. */
export const hostWorking: Host = {
  id: "pega-we-142",
  host: "10.99.0.11",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  source: "pegasis0/rmng-template:latest",
  group: "pooled",
  linearWorkspace: "we",
  linearTicket: "WE-142",
  linearLabel: "frontend",
  displayName: "Normalize sidebar CPU to % of allowance",
  monitorState: "working",
  agentReport: "working",
  stateNote: "Refactoring usageLine to divide by the clone's cpu allowance",
};

/** Idle, bound to the "pooled" group, with an unread dot (dropped from working). */
export const hostIdle: Host = {
  id: "pega-dev-88",
  host: "10.99.0.12",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  source: "pegasis0/rmng-template:latest",
  group: "pooled",
  linearWorkspace: "dev",
  linearTicket: "DEV-88",
  displayName: "Wire up the pull-template wizard",
  monitorState: "idle",
  stateNote: "Done — awaiting review",
  unread: true,
};

/** Offline (wrapper unreachable), bound to the "team" group. */
export const hostOffline: Host = {
  id: "pega-hh-7",
  host: "10.99.0.13",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  group: "team",
  displayName: "Database migration spike",
  monitorState: "idle",
  stateNote: "Container stopped",
};

/** A managed scratch box with no account group bound (no inference). */
export const hostNoToken: Host = {
  id: "scratch-box",
  host: "10.99.0.20",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  monitorState: "idle",
  stateNote: "Manual scratch container",
};

/** A plain unmanaged row (no container) — only deletable, no commit/account actions. */
export const hostUnmanaged: Host = {
  id: "legacy-desktop",
  host: "192.168.1.50",
  port: 3389,
  username: "admin",
  password: "",
  monitorState: "idle",
};

/** A managed clone bound to the "team" group. Exercises the two-line sidebar layout
 *  (binding line + a metric-only line, CPU on the first / MEM on the second). */
export const hostDualProvider: Host = {
  id: "pega-dual-9",
  host: "10.99.0.14",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  source: "pegasis0/rmng-template:latest",
  group: "team",
  linearWorkspace: "we",
  linearTicket: "WE-207",
  displayName: "Port the encoder path to the new VA surface pool",
  monitorState: "working",
  agentReport: "working",
  stateNote: "Wiring the group picker into the clone modal",
};

export const hosts: Host[] = [
  hostWorking,
  hostIdle,
  hostOffline,
  hostNoToken,
  hostUnmanaged,
  hostDualProvider,
];
export const hostIds: string[] = hosts.map((h) => h.id);

// --- live container usage (the volatile `stats` SSE map) --------------------

export const stats: Record<string, ContainerStats> = {
  [hostWorking.id]: {
    cpuPct: 640, // ÷ cloneCpus(16) → 40% of the clone's allowance
    memUsed: BigInt(Math.round(5.1 * GiB)),
    memLimit: BigInt(32 * GiB),
    dockerDiskUsed: BigInt(Math.round(91.3 * GiB)),
  },
  [hostIdle.id]: {
    cpuPct: 12,
    memUsed: BigInt(Math.round(1.4 * GiB)),
    memLimit: BigInt(32 * GiB),
    dockerDiskUsed: BigInt(Math.round(91.3 * GiB)),
  },
  [hostNoToken.id]: {
    cpuPct: 3,
    memUsed: BigInt(Math.round(0.6 * GiB)),
    memLimit: BigInt(32 * GiB),
    dockerDiskUsed: BigInt(Math.round(91.3 * GiB)),
  },
  [hostDualProvider.id]: {
    cpuPct: 288, // ÷ cloneCpus(16) → 18% of the clone's allowance
    memUsed: BigInt(Math.round(3.2 * GiB)),
    memLimit: BigInt(32 * GiB),
    dockerDiskUsed: BigInt(Math.round(91.3 * GiB)),
  },
};

// --- account groups + their usage (display-only) ---------------------------
// Under the group-proxy model, usage is grouped by pool. Account ids are group-scoped
// (`<group>|<email>`) since the same email can be authed into several groups.

const usage = (
  group: string,
  a: Omit<ClaudeUsage, "id" | "active"> & Partial<Pick<ClaudeUsage, "active">>,
): ClaudeUsage => ({ id: `${group}|${a.email}`, active: a.active ?? true, ...a });

export const usageGroups: GroupUsage[] = [
  {
    name: "pooled",
    accounts: [
      usage("pooled", {
        email: "alex@example.com",
        provider: "claude",
        assignable: true,
        lastUpdated: 1_700_000_000_000,
        fiveHour: { pct: 42, resetsAt: null },
        sevenDay: { pct: 61, resetsAt: null },
        fable: { pct: 8, resetsAt: null },
      }),
      usage("pooled", {
        email: "sam@example.com",
        provider: "claude",
        active: false,
        assignable: true,
        lastUpdated: 1_700_000_000_000,
        fiveHour: { pct: 88, resetsAt: null },
        sevenDay: { pct: 73, resetsAt: null },
      }),
    ],
  },
  {
    name: "team",
    accounts: [
      usage("team", {
        email: "alex@openai.com",
        provider: "codex",
        assignable: false,
        lastUpdated: 1_700_000_000_000,
        // Codex exposes only a weekly (7d) limit now — the 5h window was removed upstream.
        sevenDay: { pct: 40, resetsAt: null },
        resetCredits: 3n,
      }),
    ],
  },
];

/** The configured account groups (names only) — the authoritative list for pickers. */
export const groups: Group[] = [{ name: "pooled" }, { name: "team" }];

// --- clone-source images ----------------------------------------------------

export const images: ImageInfo[] = [
  {
    id: "sha256:aaaa0000",
    reference: "pegasis0/rmng-template:latest",
    sizeBytes: BigInt(6_800_000_000),
    createdAt: "2026-06-20T12:00:00Z",
    base: true,
    createdFrom: null,
    inUseBy: [hostWorking.id, hostIdle.id],
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

/** A running delete op targeting an existing host (shows the row's busy state). */
export const deleteOperation: Operation = {
  id: "op-delete-1",
  kind: "delete",
  target: hostIdle.id,
  status: "running",
  step: "stopping",
  pct: 30,
  message: "Stopping container…",
  log: ["stopping container"],
  startedAt: 1_700_000_000_000,
};

// --- redacted app config (for the Settings story) --------------------------

export const appConfig: AppConfigRedacted = {
  listen: { web: 9000, video: 9001, cloneMcp: 9002, daemonMcp: 9004, forward: 9005, bastion: 2222 },
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
  groups: [{ name: "pooled" }, { name: "team" }],
  presets: [
    {
      name: "webapp",
      labels: ["frontend", "webapp"],
      linearKeySet: true,
      vars: [{ key: "NODE_ENV", value: "development" }],
      agentPlaybook: "",
    },
  ],
  chroma: "yuv420",
  ssh: {
    authorizedKeys: ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeStorybookDemoKeyOnly alex@laptop"],
    publicHost: "rmng.example.com",
  },
  detectorInferenceUrl: "http://detector.internal:8080",
  agentPlaybook: "# Desktop agent — operating notes\n\n(sample playbook)\n",
};

