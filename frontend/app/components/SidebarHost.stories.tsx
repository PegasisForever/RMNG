import { DndContext } from "@dnd-kit/core";
import { SortableContext, verticalListSortingStrategy } from "@dnd-kit/sortable";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SidebarHost } from "./SidebarHost";
import {
  deleteOperation,
  hostIdle,
  hostIds,
  hostNoToken,
  hostOffline,
  hostUnmanaged,
  hostWorking,
  stats,
} from "~/stories/fixtures";

const meta = {
  title: "Sidebar/SidebarHost",
  component: SidebarHost,
  parameters: { layout: "padded" },
  // SidebarHost calls dnd-kit's useSortable, which reads from a DndContext +
  // SortableContext. Provide them (with every fixture id registered) so the grip
  // works and the row renders exactly as it does in the live list.
  decorators: [
    (Story) => (
      <DndContext>
        <SortableContext items={hostIds} strategy={verticalListSortingStrategy}>
          <div className="w-96 rounded-lg bg-slate-50 p-3">
            <Story />
          </div>
        </SortableContext>
      </DndContext>
    ),
  ],
  args: {
    cloneCpus: 16,
    selected: false,
    onSelect: fn(),
    onDelete: fn(),
    onCommit: fn(),
    onChangeAccount: fn(),
  },
} satisfies Meta<typeof SidebarHost>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Managed clone actively working, pinned account, live CPU/RAM. */
export const Working: Story = {
  args: { host: hostWorking, stats: stats[hostWorking.id] },
};

/** Idle, balanced within a Claude group, unread dot. */
export const Idle: Story = {
  args: { host: hostIdle, stats: stats[hostIdle.id] },
};

/** Offline (wrapper unreachable), Claude on auto. */
export const Offline: Story = {
  args: { host: hostOffline },
};

/** Managed scratch box with no Claude token installed. */
export const NoToken: Story = {
  args: { host: hostNoToken, stats: stats[hostNoToken.id] },
};

/** Plain unmanaged row — delete only (no commit / account actions). */
export const Unmanaged: Story = {
  args: { host: hostUnmanaged },
};

/** The selected (active) row. */
export const Selected: Story = {
  args: { host: hostWorking, stats: stats[hostWorking.id], selected: true },
};

/** A row with a running op targeting it (delete in progress) — busy state. */
export const Busy: Story = {
  args: { host: hostIdle, op: deleteOperation },
};

/** Overflow stress test: a very long title (wraps — continuation flows back to the
 *  left edge past the ticket badge) and a very long state note (clamped to two lines). */
export const LongTitleAndDescription: Story = {
  args: {
    host: {
      ...hostWorking,
      linearTicket: "WE-1042",
      displayName:
        "Investigate and fix the intermittent WebRTC reconnection storm when a clone's headless GNOME session restarts under sustained 4:4:4 encode load",
      stateNote:
        "Reproduced on CT 106 — the reconnect loop fires because the daemon re-applies the monitor layout before the encoder releases the previous VA surface; drafting a fix that serializes the two on the shared clone-daemon unit and adds a backoff",
    },
    stats: stats[hostWorking.id],
  },
};
