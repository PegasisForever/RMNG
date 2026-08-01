import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { CloneExistingTicketFields } from "./CloneExistingTicketFields";
import { makeClonePresets } from "./__fixtures__/presets";
import { cloneTicketUrl } from "./__fixtures__/cloneDialog";
import { resolvePreset } from "~/lib/cloneDraft";
import { parseTicketInput } from "~/lib/workspace";

/** The fields sit in the dialog's body, so the story gives them the same width. */
function Frame({ children }: { children: ReactNode }) {
  return <div className="w-[28rem]">{children}</div>;
}

const presets = makeClonePresets();

/** The same two derivations the container makes, so a story sets the ticket text and the
 *  parse and the resolved preset follow from it, exactly as they do in the dialog. */
function derive(ticket: string) {
  const parsed = parseTicketInput(ticket);
  return {
    parsed,
    preset: resolvePreset("existing", presets, { ticketPrefix: parsed?.prefix }),
  };
}

const meta = {
  title: "Clone/Components/CloneExistingTicketFields",
  component: CloneExistingTicketFields,
  parameters: { layout: "centered" },
  args: {
    ticket: "",
    ...derive(""),
    presets,
    onTicketChange: fn(),
    onSubmit: fn(),
  },
  render: (args) => (
    <Frame>
      <CloneExistingTicketFields {...args} />
    </Frame>
  ),
} satisfies Meta<typeof CloneExistingTicketFields>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Nothing pasted yet. No parse, so no badge and no preset — the line reads an em-dash rather
 *  than naming a preset the clone might not get. */
export const Empty: Story = {};

/** A pasted Linear link. The id comes out of the URL, the hostname it will become is shown
 *  next to it, and the prefix has resolved to the preset that claims it. */
export const TicketPasted: Story = {
  args: { ticket: cloneTicketUrl, ...derive(cloneTicketUrl) },
};

/** Text with no ticket id in it. The field says so under itself and the Clone button stays
 *  dead. */
export const Unparseable: Story = {
  args: { ticket: "the sidebar one", ...derive("the sidebar one") },
};

/** A ticket id whose prefix no preset claims. Blocking, not cosmetic: with no preset dropdown
 *  there is nothing to override the auto-selection with, so the line says what to fix. */
export const PrefixUnclaimed: Story = {
  args: { ticket: "ZZZ-9", ...derive("ZZZ-9") },
};

/** No presets configured at all. A prefix that resolves to nothing is then expected rather
 *  than wrong, so the line stays neutral instead of blaming the ticket. */
export const NoPresets: Story = {
  args: { ticket: "WE-142", parsed: parseTicketInput("WE-142"), preset: undefined, presets: [] },
};
