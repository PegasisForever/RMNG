import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { Sidebar } from "./Sidebar";
import { cloneOperation, hosts, lxcStats, stats, tokens, usageGroups } from "~/stories/fixtures";

const meta = {
  title: "Sidebar/Sidebar",
  component: Sidebar,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story) => (
      // Full-height shell so the static (≥ lg) panel sits against the app bg.
      <div className="h-screen bg-white dark:bg-slate-950">
        <Story />
      </div>
    ),
  ],
  args: {
    open: true,
    usageGroups,
    hosts,
    stats,
    lxcStats,
    tokens,
    operations: [],
    selectedId: hosts[0].id,
    sshPublicHost: "rmng.example.com",
    bastionPort: 2222,
    presetNames: ["Default", "Focus"],
    activeLayout: "Default",
    onActivateLayout: fn(),
    onOpenSettings: fn(),
    onOpenClone: fn(),
    onCreateGroup: fn(),
    onAddAccount: fn(),
    onDeleteGroup: fn(),
    onRefresh: fn(),
    onSelectClone: fn(),
    onDeleteClone: fn(),
    onCommitClone: fn(),
    onChangeAccountClone: fn(),
    onPortForwardClone: fn(),
    onArchiveClone: fn(),
    onUnarchiveClone: fn(),
    onReorder: fn(),
  },
} satisfies Meta<typeof Sidebar>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The full panel with clones and accounts. Drag-reorder is wired to local state so
 *  the list actually reorders (and still logs the `onReorder` action). */
export const Default: Story = {
  render: (args) => {
    const [order, setOrder] = useState(args.hosts.map((h) => h.id));
    const byId = new Map(args.hosts.map((h) => [h.id, h]));
    const ordered = order.flatMap((id) => {
      const h = byId.get(id);
      return h ? [h] : [];
    });
    return (
      <Sidebar
        {...args}
        hosts={ordered}
        onReorder={(ids) => {
          setOrder(ids);
          args.onReorder(ids);
        }}
      />
    );
  },
};

/** Fresh install — no clones or groups yet. */
export const Empty: Story = {
  args: { hosts: [], usageGroups: [], lxcStats: null, selectedId: null },
};

/** A clone in flight — the Activity section renders and + Clone is disabled. */
export const WithActivity: Story = {
  args: { operations: [cloneOperation] },
};

/** Retained clones are separated from active, drag-reorderable clones. */
export const WithArchivedClones: Story = {
  args: {
    hosts: [
      ...hosts,
      { ...hosts[0], id: "archived-clone", displayName: "Archived clone", archived: true },
    ],
  },
};
