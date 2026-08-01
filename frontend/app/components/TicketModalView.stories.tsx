import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { MarkdownEditorView } from "./MarkdownEditorView";
import { TicketModalView } from "./TicketModalView";
import { makeClonePresets } from "./__fixtures__/presets";

/** Resolves after a beat, so the button's "Creating…" state is visible in the story. */
const slowCreate = fn(
  (t: unknown) => new Promise((resolve) => setTimeout(() => resolve(t), 800)),
);

/** The real description slot, on a stub upload: a pasted image really appears, from the copy
 *  already in the browser's memory. The live field posts to /api/upload instead. */
const descriptionEditor = (
  <MarkdownEditorView
    onChange={fn()}
    uploadFile={async (file: File) => URL.createObjectURL(file)}
    placeholder="What needs doing — paste images, format freely"
  />
);

const meta = {
  title: "Board/Components/TicketModalView",
  component: TicketModalView,
  parameters: { layout: "fullscreen" },
  args: {
    presets: makeClonePresets(),
    description: "",
    descriptionEditor,
    onClose: fn(),
    onCreate: slowCreate,
  },
  /** The dialog positions itself over the page, so the story gives it a board-coloured one
   *  to sit on rather than a white void. */
  render: (args) => (
    <div className="h-screen bg-slate-50 dark:bg-slate-950">
      <TicketModalView {...args} />
    </div>
  ),
} satisfies Meta<typeof TicketModalView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The normal case: WE is the first team key, and its preset holds a Linear key. */
export const Default: Story = {};

/** The chosen team's preset carries no Linear key, so nothing can open a ticket there. The
 *  dialog says which preset and where to fix it, and the create button stays down. Pick OPS
 *  in the dropdown to see it. */
export const NoLinearKey: Story = {
  args: { presets: makeClonePresets().filter((p) => !p.linearKeySet) },
};

/** No preset declares a team key at all, which is the first-run state. */
export const NoTeams: Story = {
  args: { presets: [] },
};

/** Linear refused it. The dialog keeps everything typed and shows what came back. */
export const CreateFails: Story = {
  args: {
    onCreate: fn((_t: unknown) =>
      Promise.reject(new Error("Linear refused a new ticket in WE")),
    ),
  },
};
