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
    // The seeded form, rebuilt per story: the monitors array is what the editor replaces on
    // every edit, so one draft behind every story is how an edit in one leaks into the next.
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

/** The normal first run: a two-monitor arrangement carried over from the config. */
export const Default: Story = { args: { draft: makeSetupDraft() } };

/** No prefix typed, so the example hostname under the field falls back to the one the server
 *  would use. */
export const BlankPrefix: Story = {
  args: { draft: makeSetupDraft({ hostnamePrefix: "" }) },
};

/** A host with one screen. The preview draws a single box and Remove is dead: an arrangement
 *  with no monitors is not one a clone can boot. */
export const SingleMonitor: Story = {
  args: {
    draft: makeSetupDraft({
      monitors: [{ width: 1920, height: 1080, x: 0, y: 0, primary: true }],
    }),
  },
};

/** The step wired to local state instead of the wizard: every field really edits and the
 *  monitor editor really rearranges the preview. */
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
