import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { CloneNewTicketFields } from "./CloneNewTicketFields";
import { MarkdownEditorView } from "./MarkdownEditorView";
import { makeClonePresets } from "./__fixtures__/presets";
import { teamKeysOf } from "~/lib/cloneDraft";

/** The fields sit in the dialog's body, so the story gives them the same width. */
function Frame({ children }: { children: ReactNode }) {
  return <div className="w-[28rem]">{children}</div>;
}

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
  title: "Clone/Components/CloneNewTicketFields",
  component: CloneNewTicketFields,
  parameters: { layout: "centered" },
  args: {
    teamKeys: teamKeysOf(makeClonePresets()),
    team: "we",
    title: "",
    description: descriptionEditor,
    onTeamChange: fn(),
    onTitleChange: fn(),
  },
  render: (args) => (
    <Frame>
      <CloneNewTicketFields {...args} />
    </Frame>
  ),
} satisfies Meta<typeof CloneNewTicketFields>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The tab as it opens: a team key already chosen (the first one), an empty title, and the
 *  editor on its placeholder. Each option reads "WE · webapp", because picking the team is
 *  also picking the preset. */
export const Default: Story = {};

/** Mid-typing, on the second team. Nothing else about the tab changes with the team except
 *  which preset the clone will get. */
export const Filled: Story = {
  args: { team: "dev", title: "Spike: swap the encoder to VA-API" },
};

/** No preset declares a team key, so there is no team to open an issue in and no preset to
 *  open it with. The dropdown is replaced by what to go and fix. */
export const NoTeamKeys: Story = {
  args: { teamKeys: [], team: "" },
};
