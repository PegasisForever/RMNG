import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { SetupServerStep } from "./SetupServerStep";
import type { SetupDraft } from "~/lib/setupDraft";
import { makeSetupDraft } from "./__fixtures__/appConfig";

/** The step sits in the wizard's card, so the story gives it that column width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

const meta = {
  title: "Setup/Components/SetupServerStep",
  component: SetupServerStep,
  parameters: { layout: "centered" },
  args: {
    draft: makeSetupDraft(),
    onDraftChange: fn(),
  },
  render: (args) => (
    <Frame>
      <SetupServerStep {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SetupServerStep>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The normal first run. */
export const Default: Story = { args: { draft: makeSetupDraft() } };

/** No prefix typed, so the example hostname under the field falls back to the one the server
 *  would use. */
export const BlankPrefix: Story = {
  args: { draft: makeSetupDraft({ hostnamePrefix: "" }) },
};

/** The step wired to local state instead of the wizard: every field really edits. */
export const Interactive: Story = {
  args: { draft: makeSetupDraft() },
  render: function Render(args) {
    const [draft, setDraft] = useState(args.draft);
    return (
      <Frame>
        <SetupServerStep
          {...args}
          draft={draft}
          onDraftChange={<K extends keyof SetupDraft>(key: K, value: SetupDraft[K]) => {
            setDraft((d) => ({ ...d, [key]: value }));
            args.onDraftChange(key, value);
          }}
        />
      </Frame>
    );
  },
};
