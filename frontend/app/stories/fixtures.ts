// Shared, API-free sample data for the Storybook stories. Nothing here touches the
// network — the components are all dependency-injected, so a story is just "this
// fixture + these callbacks". Kept in one place so the Sidebar / SidebarHost /
// Settings stories stay consistent.

import type { ClaudeUsage, Host, Operation } from "~/lib/types";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { ImageInfo } from "~/lib/wire/ImageInfo";

const GiB = 1024 ** 3;

// --- hosts (each covers a distinct visual state) ---------------------------

/** A managed clone actively working, pinned to a specific Claude account, on a ticket. */
export const hostWorking: Host = {
  id: "pega-we-142",
  host: "10.99.0.11",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  source: "rmng/template:base",
  claudeAccountEmail: "alex@example.com",
  claudeSelection: "alex@example.com",
  linearWorkspace: "we",
  linearTicket: "WE-142",
  linearLabel: "frontend",
  displayName: "Normalize sidebar CPU to % of allowance",
  monitorState: "working",
  agentReport: "working",
  stateNote: "Refactoring usageLine to divide by the clone's cpu allowance",
};

/** Idle, balanced within a Claude group, with an unread dot (dropped from working). */
export const hostIdle: Host = {
  id: "pega-dev-88",
  host: "10.99.0.12",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  source: "rmng/template:base",
  claudeGroup: "pooled",
  claudeAccountEmail: "sam@example.com",
  claudeSelection: "group:pooled",
  linearWorkspace: "dev",
  linearTicket: "DEV-88",
  displayName: "Wire up the pull-template wizard",
  monitorState: "idle",
  stateNote: "Done — awaiting review",
  unread: true,
};

/** Offline (wrapper unreachable), Claude on auto-select. */
export const hostOffline: Host = {
  id: "pega-hh-7",
  host: "10.99.0.13",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  claudeSelection: "auto",
  claudeAccountEmail: "auto-picked@example.com",
  displayName: "Database migration spike",
  monitorState: "idle",
  stateNote: "Container stopped",
};

/** A managed scratch box with no Claude token installed. */
export const hostNoToken: Host = {
  id: "scratch-box",
  host: "10.99.0.20",
  port: 3389,
  username: "pega",
  password: "",
  managed: true,
  claudeSelection: "none",
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

export const hosts: Host[] = [hostWorking, hostIdle, hostOffline, hostNoToken, hostUnmanaged];
export const hostIds: string[] = hosts.map((h) => h.id);

// --- live container usage (the volatile `stats` SSE map) --------------------

export const stats: Record<string, ContainerStats> = {
  [hostWorking.id]: {
    cpuPct: 640, // ÷ cloneCpus(16) → 40% of the clone's allowance
    memUsed: BigInt(Math.round(5.1 * GiB)),
    memLimit: BigInt(32 * GiB),
  },
  [hostIdle.id]: {
    cpuPct: 12,
    memUsed: BigInt(Math.round(1.4 * GiB)),
    memLimit: BigInt(32 * GiB),
  },
  [hostNoToken.id]: {
    cpuPct: 3,
    memUsed: BigInt(Math.round(0.6 * GiB)),
    memLimit: BigInt(32 * GiB),
  },
};

// --- Claude accounts (display-only usage view) -----------------------------

export const claudeAccounts: ClaudeUsage[] = [
  {
    id: "alex@example.com|org-a",
    email: "alex@example.com",
    provider: "claude",
    active: true,
    assignable: true,
    lastUpdated: 1_700_000_000_000,
    fiveHour: { pct: 42, resetsAt: null },
    sevenDay: { pct: 61, resetsAt: null },
  },
  {
    id: "sam@example.com|org-a",
    email: "sam@example.com",
    provider: "claude",
    active: false,
    assignable: true,
    lastUpdated: 1_700_000_000_000,
    fiveHour: { pct: 88, resetsAt: null },
    sevenDay: { pct: 73, resetsAt: null },
  },
];

// --- clone-source images ----------------------------------------------------

export const images: ImageInfo[] = [
  {
    id: "sha256:aaaa0000",
    reference: "rmng/template:base",
    sizeBytes: BigInt(6_800_000_000),
    createdAt: "2026-06-20T12:00:00Z",
    base: true,
    createdFrom: null,
    inUseBy: [hostWorking.id, hostIdle.id],
  },
  {
    id: "sha256:bbbb1111",
    reference: "rmng/template:node20",
    sizeBytes: BigInt(7_200_000_000),
    createdAt: "2026-06-28T09:30:00Z",
    base: false,
    createdFrom: "rmng/template:base",
    inUseBy: [],
  },
];

// --- operations -------------------------------------------------------------

/** A running clone op (drives the Activity list + disables the + Clone button). */
export const cloneOperation: Operation = {
  id: "op-clone-1",
  kind: "clone",
  target: "pega-per-9",
  source: "rmng/template:base",
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
  listen: { web: 9000, video: 9001, cloneMcp: 9002, globalMcp: 9003, daemonMcp: 9004 },
  agentPort: 4096,
  dataDir: "/data",
  staticDir: "",
  cloneSocket: "/srv/rmng-sock/clones.sock",
  setupComplete: true,
  monitors: [
    { width: 2560, height: 1440, x: 0, y: 0, primary: true },
    { width: 1920, height: 1080, x: 2560, y: 0, primary: false },
  ],
  docker: {
    socket: "/var/run/docker.sock",
    subnet: "10.99.0.0/24",
    hostnamePrefix: "pega-",
    cloneCpus: 16,
    cloneMemoryMb: 32768,
    templateReference: "pegasis0/rmng-template:latest",
  },
  claude: {
    pollSecs: BigInt(600),
    pinnedEmail: "alex@example.com",
    autoSwapOnExhaustion: false,
  },
  cloneGroups: [{ name: "pooled", accounts: ["alex@example.com", "sam@example.com"] }],
  presets: [
    {
      name: "webapp",
      labels: ["frontend", "webapp"],
      linearKeySet: true,
      vars: [{ key: "NODE_ENV", value: "development" }],
    },
  ],
  chroma: "yuv420",
  detectorInferenceUrl: "http://detector.internal:8080",
};

/** Emails a Claude group can draw from (the Settings pool). */
export const accountEmails: string[] = claudeAccounts.map((a) => a.email);
