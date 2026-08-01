import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { EffectBadge, Field, Secret, Section, settingsInput } from "./SettingsFields";

const meta = {
  title: "Settings/Components/SettingsFields",
  component: Secret,
  parameters: { layout: "centered" },
  args: {
    label: "Linear API key",
    set: true,
    value: "",
    onChange: fn(),
  },
  render: (args) => (
    <div className="w-[36rem] p-4">
      <Secret {...args} />
    </div>
  ),
} satisfies Meta<typeof Secret>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The server holds a key. The input stays blank, meaning "keep it", and the badge says so —
 *  which is the whole point of a write-only field: there is nothing to read back. */
export const SecretSet: Story = {};

/** Nothing stored. The placeholder says "not set" and anything typed becomes the key. */
export const SecretUnset: Story = {
  args: { set: false },
};

/** A key being replaced. The characters are masked, so the badge is still the only thing
 *  saying whether one was there before. */
export const SecretBeingTyped: Story = {
  render: function Render(args) {
    const [value, setValue] = useState("lin_api_pasted_here");
    return (
      <div className="w-[36rem] p-4">
        <Secret {...args} value={value} onChange={setValue} />
      </div>
    );
  },
};

/** The three effect badges, on the section header where the panel puts them. `immediate`
 *  applies on save; `restart` needs the control-server restarted; `one-time` was baked in at
 *  first-run setup and cannot change at all. */
export const Effects: Story = {
  render: () => (
    <div className="w-[36rem] space-y-2 p-4">
      <Section title="Layout presets" effect="immediate" hint="Applies the moment you save.">
        <Field label="Preset name">
          <input className={settingsInput} defaultValue="Dual 1440p" />
        </Field>
      </Section>
      <Section title="Video" effect="restart" hint="Needs the control-server restarted." >
        <div className="flex items-center gap-2 text-xs text-slate-500 dark:text-slate-400">
          Ports and the encoder are wired once at startup <EffectBadge effect="restart" />
        </div>
      </Section>
      <Section title="Clone network subnet" effect="one-time" hint="Baked in at first-run setup.">
        <div className="flex items-center gap-2 text-xs text-slate-500 dark:text-slate-400">
          Read-only from here on <EffectBadge effect="one-time" />
        </div>
      </Section>
    </div>
  ),
};
