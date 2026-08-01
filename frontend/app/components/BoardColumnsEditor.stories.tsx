import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { BoardColumnsEditor } from "./BoardColumnsEditor";
import { newColumnId, removeColumn } from "~/lib/board";
import { makeBoardColumns } from "./__fixtures__/board";

/** The editor sits in the settings panel's body, so the story gives it the same width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

const meta = {
  title: "Settings/Components/BoardColumnsEditor",
  component: BoardColumnsEditor,
  parameters: { layout: "centered" },
  args: {
    columns: makeBoardColumns(),
    counts: { todo: 3, doing: 3, blocked: 1, archived: 0 },
    onAddColumn: fn(),
    onRenameColumn: fn(),
    onSetArchive: fn(),
    onDeleteColumn: fn(),
    onReorderColumns: fn(),
  },
  render: (args) => (
    <Frame>
      <BoardColumnsEditor {...args} />
    </Frame>
  ),
} satisfies Meta<typeof BoardColumnsEditor>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The board as it usually stands: three working lanes plus the archive lane, each with the
 *  clones it holds. Deleting a lane with clones in it says where they go. */
export const Default: Story = {};

/** A rig whose board has been emptied out. The list is replaced by the line that says to add
 *  one, and the add row below is the only way back. */
export const Empty: Story = {
  args: { columns: [] },
};

/** Columns that hold nothing yet, which is a fresh board. Every badge reads 0 and the delete
 *  buttons drop the "its N clones move to the first one" warning. */
export const AllEmpty: Story = {
  args: {
    columns: makeBoardColumns().map((c) => ({ ...c, cloneIds: [] })),
    counts: undefined,
  },
};

/** Wired to local state: adding, renaming, ticking archive, reordering by the grip and
 *  deleting all really happen, and each still logs to the Actions panel. */
export const Interactive: Story = {
  render: function Render(args) {
    const [columns, setColumns] = useState(args.columns);
    return (
      <Frame>
        <BoardColumnsEditor
          {...args}
          columns={columns}
          onAddColumn={(title) => {
            setColumns((prev) => [
              ...prev,
              { id: newColumnId(title, prev), title, cloneIds: [], archive: false },
            ]);
            args.onAddColumn(title);
          }}
          onRenameColumn={(id, title) => {
            setColumns((prev) => prev.map((c) => (c.id === id ? { ...c, title } : c)));
            args.onRenameColumn(id, title);
          }}
          onSetArchive={(id, archive) => {
            setColumns((prev) => prev.map((c) => (c.id === id ? { ...c, archive } : c)));
            args.onSetArchive(id, archive);
          }}
          onDeleteColumn={(id) => {
            setColumns((prev) => removeColumn(prev, id));
            args.onDeleteColumn(id);
          }}
          onReorderColumns={(ids) => {
            setColumns((prev) =>
              ids.flatMap((id) => {
                const column = prev.find((c) => c.id === id);
                return column ? [column] : [];
              }),
            );
            args.onReorderColumns(ids);
          }}
        />
      </Frame>
    );
  },
};
