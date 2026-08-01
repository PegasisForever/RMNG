import { DndContext } from "@dnd-kit/core";
import { SortableContext, verticalListSortingStrategy } from "@dnd-kit/sortable";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SidebarClone } from "./SidebarClone";
import {
  cloneDualProvider,
  cloneIdle,
  cloneIds,
  cloneNoToken,
  cloneOffline,
  cloneUnmanaged,
  cloneWorking,
  makeCloneIdle,
  makeCloneWorking,
} from "./__fixtures__/clones";
import { deleteOperation } from "./__fixtures__/operations";
import { cloneTokens, stats } from "./__fixtures__/stats";

const meta = {
  title: "Sidebar/Components/SidebarClone",
  component: SidebarClone,
  parameters: { layout: "padded" },
  // SidebarClone calls dnd-kit's useSortable, which reads from a DndContext +
  // SortableContext. Provide them (with every fixture id registered) so the grip
  // works and the row renders exactly as it does in the live list.
  decorators: [
    (Story) => (
      <DndContext>
        <SortableContext items={cloneIds} strategy={verticalListSortingStrategy}>
          <div className="w-96 rounded-lg bg-slate-50 p-3 dark:bg-slate-900">
            <Story />
          </div>
        </SortableContext>
      </DndContext>
    ),
  ],
  args: {
    sshPublicHost: "rmng.example.com",
    bastionPort: 2222,
    selected: false,
    onSelect: fn(),
    onDelete: fn(),
    onCommit: fn(),
    onChangeAccount: fn(),
    onPortForward: fn(),
    onArchive: fn(),
    onUnarchive: fn(),
    // The clipboard write and the jump to Linear are the container's, so the story stands in
    // for both. The copy answers `true` because that is what a working clipboard answers, and
    // it is what makes the row show "Copied!" without a story touching the real one.
    onCopySshCommand: fn(async () => true),
    // No story to link to: "Open in Linear" goes to linear.app, which Storybook has nothing to
    // show for. `fn()` logs the URL in the Actions panel, which is the whole of what there is
    // to see.
    onOpenInLinear: fn(),
  },
} satisfies Meta<typeof SidebarClone>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Managed clone actively working, pinned account, live CPU/RAM, and a lit fable badge. */
export const Working: Story = {
  args: {
    clone: cloneWorking,
    stats: stats[cloneWorking.id],
    tokens: cloneTokens[cloneWorking.id],
  },
};

/** Idle, balanced within a Claude group, unread dot. */
export const Idle: Story = {
  args: { clone: cloneIdle, stats: stats[cloneIdle.id], tokens: cloneTokens[cloneIdle.id] },
};

/** Both providers: a pinned Claude account on line 1 and a Codex group on line 2. Each row
 *  carries the clone's total (↑ then ↓) plus CPU or MEM, with the ⋮ spanning both. */
export const DualProvider: Story = {
  args: {
    clone: cloneDualProvider,
    stats: stats[cloneDualProvider.id],
    tokens: cloneTokens[cloneDualProvider.id],
  },
};

/** Offline (wrapper unreachable), Claude on auto. */
export const Offline: Story = {
  args: { clone: cloneOffline },
};

/** Managed scratch box with no Claude token installed. Its sub-1k totals render as plain
 *  integers rather than being rounded to `0.8k`. */
export const NoToken: Story = {
  args: {
    clone: cloneNoToken,
    stats: stats[cloneNoToken.id],
    tokens: cloneTokens[cloneNoToken.id],
  },
};

/** Plain unmanaged row — delete only (no commit / account actions). */
export const Unmanaged: Story = {
  args: { clone: cloneUnmanaged },
};

/** Retained clone: no live CPU/RAM, but its token totals stay — the work already happened. */
export const Archived: Story = {
  args: { clone: makeCloneIdle({ archived: true }), tokens: cloneTokens[cloneIdle.id] },
};

/** The selected (active) row. */
export const Selected: Story = {
  args: {
    clone: cloneWorking,
    stats: stats[cloneWorking.id],
    tokens: cloneTokens[cloneWorking.id],
    selected: true,
  },
};

/** Both clipboard paths refused, which is what an insecure context with `execCommand` blocked
 *  does. The menu row says so and stays open a beat longer, so the command in its tooltip can
 *  be selected and copied by hand. Open the ⋮ menu and pick "Copy SSH command" to see it. */
export const CopySshRefused: Story = {
  args: {
    clone: cloneWorking,
    stats: stats[cloneWorking.id],
    tokens: cloneTokens[cloneWorking.id],
    onCopySshCommand: fn(async () => false),
  },
};

/** A row with a running op targeting it (delete in progress) — busy state. */
export const Busy: Story = {
  args: { clone: cloneIdle, op: deleteOperation },
};

/** Overflow stress test: a very long title wraps back to the left edge past its ticket badge. */
export const LongTitleAndDescription: Story = {
  args: {
    clone: makeCloneWorking({
      linearTicket: "WE-1042",
      displayName:
        "Investigate and fix the intermittent WebRTC reconnection storm when a clone's headless GNOME session restarts under sustained 4:4:4 encode load",
    }),
    stats: stats[cloneWorking.id],
  },
};

/** Compact port-forward chips under the clone metadata — one `remote→local` chip per rule
 *  with a live status dot, covering every state: listening (with active-conn count),
 *  error, offline (no runtime yet), and a muted rule toggled off. */
export const WithForwards: Story = {
  args: {
    clone: makeCloneWorking({
      forwards: [
        { id: "f8080", remotePort: 3000, localPort: 8080, enabled: true, label: null },
        { id: "f9000", remotePort: 9000, localPort: 9000, enabled: true, label: null },
        { id: "f5433", remotePort: 5432, localPort: 5433, enabled: true, label: null },
        { id: "f7000", remotePort: 7000, localPort: 7000, enabled: false, label: null },
      ],
    }),
    stats: stats[cloneWorking.id],
    forwardRuntime: [
      { id: "f8080", state: "listening", error: null, activeConns: 2 },
      { id: "f9000", state: "error", error: "connection refused", activeConns: 0 },
      // f5433 has no runtime entry → offline; f7000 is disabled → muted.
    ],
  },
};
