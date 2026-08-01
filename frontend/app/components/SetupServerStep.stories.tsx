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
    portsOpen: false,
    onPortsOpenChange: fn(),
  },
  render: (args) => (
    <Frame>
      <SetupServerStep {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SetupServerStep>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The normal first run: a two-monitor arrangement carried over from the config, and the
 *  ports folded away because almost nobody changes them. */
export const Default: Story = { args: { draft: makeSetupDraft() } };

/** Ports expanded. Four numbers that make the server unreachable if they are wrong, which is
 *  why they are behind a click. */
export const PortsOpen: Story = {
  args: { draft: makeSetupDraft(), portsOpen: true },
};

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

/** The step wired to local state instead of the wizard: every field really edits, the monitor
 *  editor really rearranges the preview, and the ports block really folds. */
export const Interactive: Story = {
  args: { draft: makeSetupDraft() },
  render: function Render(args) {
    const [draft, setDraft] = useState(args.draft);
    const [portsOpen, setPortsOpen] = useState(args.portsOpen);
    return (
      <Frame>
        <SetupServerStep
          {...args}
          draft={draft}
          onDraftChange={<K extends keyof SetupDraft>(key: K, value: SetupDraft[K]) => {
            setDraft((d) => ({ ...d, [key]: value }));
            args.onDraftChange(key, value);
          }}
          portsOpen={portsOpen}
          onPortsOpenChange={(open) => {
            setPortsOpen(open);
            args.onPortsOpenChange(open);
          }}
        />
      </Frame>
    );
  },
};
