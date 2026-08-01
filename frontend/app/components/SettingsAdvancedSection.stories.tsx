import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { SettingsAdvancedSection } from "./SettingsAdvancedSection";
import { makeSettingsDraft } from "./__fixtures__/appConfig";

/** The section sits in the panel's body, so the story gives it the same width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

/** The ports-and-directories half of the form, straight off the seeded draft. */
function base() {
  const draft = makeSettingsDraft();
  return {
    listen: draft.listen,
    agentPort: draft.agentPort,
    dataDir: draft.dataDir,
    staticDir: draft.staticDir,
    cloneSocket: draft.cloneSocket,
  };
}

const meta = {
  title: "Settings/Components/SettingsAdvancedSection",
  component: SettingsAdvancedSection,
  parameters: { layout: "centered" },
  args: {
    ...base(),
    open: false,
    onOpenChange: fn(),
    onListenChange: fn(),
    onAgentPortChange: fn(),
    onStaticDirChange: fn(),
  },
  render: (args) => (
    <Frame>
      <SettingsAdvancedSection {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SettingsAdvancedSection>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Collapsed, which is how the panel opens. Almost nobody touches these, and two of them
 *  cannot be touched at all. */
export const Collapsed: Story = {};

/** Expanded. The two startup-bound ports carry a restart badge, the two that clones bake in
 *  say the number they have to match, and the two container paths are read-only. */
export const Expanded: Story = {
  args: { ...base(), open: true },
};

/** The static directory pointed at an unpacked frontend, which is what a developer does to
 *  serve their own build instead of the one embedded in the image. */
export const CustomStaticDir: Story = {
  args: { ...base(), open: true, staticDir: "/data/frontend-build" },
};

/** Wired to local state, so the expander really opens and the ports really edit. */
export const Interactive: Story = {
  args: { ...base() },
  render: function Render(args) {
    const [open, setOpen] = useState(args.open);
    const [listen, setListen] = useState(args.listen);
    const [agentPort, setAgentPort] = useState(args.agentPort);
    const [staticDir, setStaticDir] = useState(args.staticDir);
    return (
      <Frame>
        <SettingsAdvancedSection
          {...args}
          open={open}
          listen={listen}
          agentPort={agentPort}
          staticDir={staticDir}
          onOpenChange={(next) => {
            setOpen(next);
            args.onOpenChange(next);
          }}
          onListenChange={(next) => {
            setListen(next);
            args.onListenChange(next);
          }}
          onAgentPortChange={(next) => {
            setAgentPort(next);
            args.onAgentPortChange(next);
          }}
          onStaticDirChange={(next) => {
            setStaticDir(next);
            args.onStaticDirChange(next);
          }}
        />
      </Frame>
    );
  },
};
