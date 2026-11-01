import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { makeBoardColumns } from "~/components/__fixtures__/board";
import { cloneWorking, hosts, makeCloneOffline } from "~/components/__fixtures__/clones";
import { deleteOperation } from "~/components/__fixtures__/operations";
import { cloneTokens, stats } from "~/components/__fixtures__/stats";

import { inTabFrame } from "./__fixtures__/frame";
import { ClonesView } from "./ClonesView";

const archived = makeCloneOffline({ id: "pega-old-3", displayName: "Old spike", archived: true, managed: true });

const meta = {
  title: "Desk/Control panel/ClonesView",
  component: ClonesView,
  decorators: [inTabFrame],
  parameters: { layout: "centered" },
  args: {
    columns: makeBoardColumns(),
    clones: [...hosts, archived],
    cloneTickets: {},
    stats,
    cloneTokens,
    operations: [],
    mutedClones: [],
    selectedId: cloneWorking.id,
    notesId: null,
    onSelect: fn(),
    onOpen: fn(),
    onRename: fn(),
    onNewClone: fn(),
    onMoveToColumn: fn(),
    onMove: fn(),
    onOpenInLinear: fn(),
    onChangeAccount: fn(),
    onRebase: fn(),
    onPortForward: fn(),
    onCopySsh: fn(async () => true),
    onToggleMute: fn(),
    onArchive: fn(),
    onUnarchive: fn(),
    onDelete: fn(),
  },
} satisfies Meta<typeof ClonesView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The board's columns as sections; the clone on the screen is lit. ⋯ opens a clone's actions. */
export const Default: Story = {};

/** One clone muted, one being deleted. */
export const MutedAndBusy: Story = {
  args: { mutedClones: [cloneWorking.id], operations: [deleteOperation] },
};

/** No clones yet. */
export const Empty: Story = { args: { clones: [], columns: [] } };
