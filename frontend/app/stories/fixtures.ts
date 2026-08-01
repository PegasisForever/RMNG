// Shared, API-free sample data for the Storybook stories. Nothing here touches the
// network — the components are all dependency-injected, so a story is just "this
// fixture + these callbacks". Kept in one place so the Sidebar / SidebarClone /
// Settings stories stay consistent.

import type { PartialBlock } from "@blocknote/core";

import type { BoardColumn } from "~/lib/board";
import type { LinearTicket } from "~/lib/tickets";
import type { ChatMessage, ClaudeUsage, Clone, Operation } from "~/lib/types";
import type { ScheduledMessage } from "~/lib/wire/ScheduledMessage";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { CloneTokens } from "~/lib/wire/CloneTokens";
import type { LxcStats } from "~/lib/wire/LxcStats";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
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

// --- board columns ----------------------------------------------------------
// The dashboard board's swim lanes. `legacy-desktop` is deliberately in none of them: an
// unfiled clone shows up in the first column, which is how a new clone reaches the board.
// `pega-we-142-helper` is in none of them either, for the opposite reason: it is a sub clone,
// so the board draws it under its parent rather than filing it anywhere.

export const boardColumns: BoardColumn[] = [
  { id: "todo", title: "Todo", cloneIds: [cloneIdle.id, cloneNoToken.id], archive: false },
  {
    id: "doing",
    title: "In progress",
    cloneIds: [cloneWorking.id, cloneDualProvider.id],
    archive: false,
  },
  { id: "blocked", title: "Blocked", cloneIds: [cloneOffline.id], archive: false },
  // Dropping a clone here archives it; dragging one out starts it again.
  { id: "archived", title: "Archived", cloneIds: [], archive: true },
];

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

// --- per-clone token totals ------------------------------------------------
// `ControlState.cloneTokens`: all-time input/output summed across both providers, plus the
// transient "this clone just used Fable" flag. Deliberately spans several orders of
// magnitude so the compact formatter's thresholds (k / M) are exercised on screen.

export const cloneTokens: Record<string, CloneTokens> = {
  [cloneWorking.id]: {
    inputTokens: BigInt(24_100_000),
    outputTokens: BigInt(890_000),
    fableActive: true,
  },
  [cloneIdle.id]: {
    inputTokens: BigInt(6_000_000),
    outputTokens: BigInt(210_000),
    fableActive: false,
  },
  [cloneDualProvider.id]: {
    inputTokens: BigInt(1_240_000),
    outputTokens: BigInt(96_400),
    fableActive: false,
  },
  // A clone that has barely run: sub-1k figures render as plain integers.
  [cloneNoToken.id]: {
    inputTokens: BigInt(812),
    outputTokens: BigInt(47),
    fableActive: false,
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

// --- agent chat -------------------------------------------------------------
// The page story renders the chat pane from these instead of the per-clone SSE stream.
// `chatNow` is the clock the view is given, so the scheduled-message labels ("Today
// 15:30") come out the same on every load.

export const chatNow = new Date(2026, 6, 27, 14, 5).getTime();

export const chatMessages: ChatMessage[] = [
  {
    id: "m1",
    role: "user",
    text: "Open the sidebar mockup in Figma and compare it against the running dashboard.",
    ts: chatNow - 9 * 60_000,
  },
  {
    id: "m2",
    role: "assistant",
    text: "Both are open side by side. The metric row is 4px tighter in the mockup and the token counts sit on the second line rather than the first.",
    ts: chatNow - 8 * 60_000,
  },
  {
    id: "m3",
    role: "user",
    text: "Make the running dashboard match, then take a screenshot.",
    ts: chatNow - 2 * 60_000,
  },
];

/** Queued for later delivery, soonest first. */
export const scheduledMessages: ScheduledMessage[] = [
  {
    id: "s1",
    at: BigInt(chatNow + 55 * 60_000),
    createdAt: BigInt(chatNow - 3 * 60_000),
    text: "Re-run the visual diff once the build lands.",
  },
  {
    id: "s2",
    at: BigInt(chatNow + 20 * 3600_000),
    createdAt: BigInt(chatNow - 3 * 60_000),
    text: "Summarize what changed in the sidebar and post it in the notes.",
  },
];

// --- clone notes ------------------------------------------------------------

/** A sample BlockNote document, so the notes pane in the page story has real content
 *  without touching /api/notes. */
export const notesBlocks: PartialBlock[] = [
  { type: "heading", props: { level: 2 }, content: "Sidebar redesign" },
  {
    type: "paragraph",
    content:
      "The clone rows carry three separate numbers (CPU, memory, tokens) and they currently compete for the same line. Give tokens their own row.",
  },
  { type: "bulletListItem", content: "Fixed-width metric labels so the arrows line up" },
  { type: "bulletListItem", content: "Provider logo shrinks to 12px in the usage rows" },
  { type: "checkListItem", props: { checked: true }, content: "Count tokens from the agent logs" },
  { type: "checkListItem", props: { checked: false }, content: "Roll sub clone activity up to the parent" },
  { type: "paragraph", content: "" },
];

// --- redacted app config (for the Settings story) --------------------------

// --- presets ----------------------------------------------------------------
// A preset's labels ARE the team keys the ticket and clone dialogs offer, so these match the
// ticket fixtures below: WE and DEV. `platform` is the one with no Linear key, which is what
// the dialogs block on.

export const presets: PresetRedacted[] = [
  {
    name: "webapp",
    labels: ["WE", "frontend"],
    linearKeySet: true,
    // A preset that defaults its clones to a pool; Codex left with no default.
    claudeAccount: "group:pooled",
    codexAccount: "",
    vars: [{ key: "NODE_ENV", value: "development" }],
    agentPlaybook: "",
    globalPrompt: "",
  },
  {
    name: "devtools",
    labels: ["DEV"],
    linearKeySet: true,
    claudeAccount: "",
    codexAccount: "",
    vars: [],
    agentPlaybook: "",
    globalPrompt: "",
  },
  {
    name: "platform",
    labels: ["OPS"],
    linearKeySet: false,
    claudeAccount: "",
    codexAccount: "",
    vars: [],
    agentPlaybook: "",
    globalPrompt: "",
  },
];

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
  presets,
  chroma: "yuv420",
  ssh: {
    authorizedKeys: ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeStorybookDemoKeyOnly alex@laptop"],
    publicHost: "rmng.example.com",
  },
  agentPlaybook: "# Desktop agent — operating notes\n\n(sample playbook)\n",
  globalPrompt: "# Working in this clone\n\n(sample shared operating memory)\n",
};


// --- Linear tickets ---------------------------------------------------------
// What the ticket column draws: the union of every configured Linear key's own issues.
// `WE-142` and `DEV-88` are deliberately here AND on a clone in `hosts`, and `DEV-104`
// appears twice as two presets sharing one key would return it. `openTickets` is what
// drops all three, so a story that skips the filter shows the bug rather than hiding it.

export const linearTickets: LinearTicket[] = [
  {
    id: "WE-301",
    title: "Encoder drops frames when a second monitor is hot-plugged",
    url: "https://linear.app/pegasis/issue/WE-301/encoder-drops-frames",
    // Linear sent its own branch name for this one, handle prefix and all; the rest fall
    // back to the derived shape.
    branchName: "alex/we-301-encoder-drops-frames-when-a-second-monitor",
    assignee: "Alex",
    dueDate: "2026-08-14",
    estimate: 3,
    description:
      "Hot-plugging a second monitor mid-session drops roughly 40 frames before the encoder settles.\n\n" +
      "## What happens\n\n" +
      "`vapostproc` renegotiates caps when the monitor set changes, and the encoder is torn down and rebuilt " +
      "while frames are still arriving. Those frames are dropped rather than queued.\n\n" +
      "## What it should do\n\n" +
      "1. Hold the incoming frames while the caps settle.\n" +
      "2. Rebuild the encoder against the new layout.\n" +
      "3. Drain the held frames.",
    children: [
      {
        id: "WE-302",
        title: "Reproduce with a scripted hot-plug",
        url: "https://linear.app/pegasis/issue/WE-302/reproduce",
        state: "done",
      },
      {
        id: "WE-303",
        title: "Hold frames across a caps renegotiation",
        url: "https://linear.app/pegasis/issue/WE-303/hold-frames",
        state: "in_progress",
      },
      {
        id: "WE-304",
        title: "Add a hot-plug case to the capture self-test",
        url: "https://linear.app/pegasis/issue/WE-304/self-test",
        state: "todo",
      },
    ],
    labels: [
      { name: "Bug", color: "#eb5757" },
      { name: "Video", color: "#0f7488" },
    ],
    state: "todo",
    team: "WE",
    priority: 1,
  },
  {
    id: "WE-288",
    title: "Board columns should remember their scroll position",
    url: "https://linear.app/pegasis/issue/WE-288/board-columns-scroll",
    labels: [{ name: "Feature", color: "#bb87fc" }],
    assignee: "Alex",
    description:
      "Scrolling a column, selecting a clone, and coming back puts you at the top again.\n\n" +
      "Store the offset per column and restore it on mount.",
    parent: {
      id: "WE-280",
      title: "Board polish",
      url: "https://linear.app/pegasis/issue/WE-280/board-polish",
      state: "in_progress",
    },
    children: [],
    state: "in_progress",
    team: "WE",
    priority: 2,
  },
  {
    id: "DEV-104",
    title: "Retry the usage poll on a 429 instead of dropping the window",
    url: "https://linear.app/pegasis/issue/DEV-104/retry-usage-poll",
    labels: [
      { name: "Feature", color: "#bb87fc" },
      { name: "Voice", color: "#4ea7fc" },
    ],
    state: "todo",
    team: "DEV",
    priority: 3,
    children: [],
  },
  {
    id: "DEV-97",
    title: "Document the bastion port in the SSH panel",
    url: "https://linear.app/pegasis/issue/DEV-97/document-bastion-port",
    labels: [{ name: "Docs", color: "#5e6ad2" }],
    state: "todo",
    team: "DEV",
    children: [],
  },
  // The same issue again, as a second key carrying the same account would return it.
  {
    id: "DEV-104",
    title: "Retry the usage poll on a 429 instead of dropping the window",
    url: "https://linear.app/pegasis/issue/DEV-104/retry-usage-poll",
    labels: [
      { name: "Feature", color: "#bb87fc" },
      { name: "Voice", color: "#4ea7fc" },
    ],
    state: "todo",
    team: "DEV",
    priority: 3,
    children: [],
  },
  // Already cloned (see `cloneWorking` / `cloneIdle`), so the filter drops both.
  {
    id: "WE-142",
    title: "Normalize sidebar CPU to % of allowance",
    url: "https://linear.app/pegasis/issue/WE-142/normalize-sidebar-cpu",
    state: "in_progress",
    team: "WE",
    priority: 2,
    labels: [],
    children: [],
  },
  {
    id: "DEV-88",
    title: "Wire up the pull-template wizard",
    url: "https://linear.app/pegasis/issue/DEV-88/pull-template-wizard",
    state: "todo",
    team: "DEV",
    labels: [],
    children: [],
  },
];
