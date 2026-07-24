import { DndContext } from "@dnd-kit/core";
import { SortableContext, verticalListSortingStrategy } from "@dnd-kit/sortable";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SidebarClone } from "./SidebarClone";
import {
  deleteOperation,
  cloneDualProvider,
  cloneIdle,
  cloneIds,
  cloneNoToken,
  cloneOffline,
  cloneUnmanaged,
  cloneWorking,
  stats,
} from "~/stories/fixtures";

const meta = {
  title: "Sidebar/SidebarClone",
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
  },
} satisfies Meta<typeof SidebarClone>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Managed clone actively working, pinned account, live CPU/RAM. */
export const Working: Story = {
  args: { clone: cloneWorking, stats: stats[cloneWorking.id] },
};

/** Idle, balanced within a Claude group, unread dot. */
export const Idle: Story = {
  args: { clone: cloneIdle, stats: stats[cloneIdle.id] },
};

/** Both providers: a pinned Claude account on line 1 and a Codex group on line 2, with
 *  CPU on the Claude line and MEM on the Codex line, and the ⋯ spanning both. */
export const DualProvider: Story = {
  args: { clone: cloneDualProvider, stats: stats[cloneDualProvider.id] },
};

/** Offline (wrapper unreachable), Claude on auto. */
export const Offline: Story = {
  args: { clone: cloneOffline },
};

/** Managed scratch box with no Claude token installed. */
export const NoToken: Story = {
  args: { clone: cloneNoToken, stats: stats[cloneNoToken.id] },
};

/** Plain unmanaged row — delete only (no commit / account actions). */
export const Unmanaged: Story = {
  args: { clone: cloneUnmanaged },
};

/** Retained clone: no live runtime actions or usage, but it remains selectable. */
export const Archived: Story = {
  args: { clone: { ...cloneIdle, archived: true } },
};

/** The selected (active) row. */
export const Selected: Story = {
  args: { clone: cloneWorking, stats: stats[cloneWorking.id], selected: true },
};

/** A row with a running op targeting it (delete in progress) — busy state. */
export const Busy: Story = {
  args: { clone: cloneIdle, op: deleteOperation },
};

/** Overflow stress test: a very long title wraps back to the left edge past its ticket badge. */
export const LongTitleAndDescription: Story = {
  args: {
    clone: {
      ...cloneWorking,
      linearTicket: "WE-1042",
      displayName:
        "Investigate and fix the intermittent WebRTC reconnection storm when a clone's headless GNOME session restarts under sustained 4:4:4 encode load",
    },
    stats: stats[cloneWorking.id],
  },
};

/** Compact port-forward chips under the clone metadata — one `remote→local` chip per rule
 *  with a live status dot, covering every state: listening (with active-conn count),
 *  error, offline (no runtime yet), and a muted rule toggled off. */
export const WithForwards: Story = {
  args: {
    clone: {
      ...cloneWorking,
      forwards: [
        { id: "f8080", remotePort: 3000, localPort: 8080, enabled: true, label: null },
        { id: "f9000", remotePort: 9000, localPort: 9000, enabled: true, label: null },
        { id: "f5433", remotePort: 5432, localPort: 5433, enabled: true, label: null },
        { id: "f7000", remotePort: 7000, localPort: 7000, enabled: false, label: null },
      ],
    },
    stats: stats[cloneWorking.id],
    forwardRuntime: [
      { id: "f8080", state: "listening", error: null, activeConns: 2 },
      { id: "f9000", state: "error", error: "connection refused", activeConns: 0 },
      // f5433 has no runtime entry → offline; f7000 is disabled → muted.
    ],
  },
};
