import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { RebaseModalView } from "./RebaseModalView";
import { makeOperation } from "./__fixtures__/operations";
import { makeClonePresets } from "./__fixtures__/presets";

const meta = {
  title: "Clone/Components/RebaseModalView",
  component: RebaseModalView,
  parameters: { layout: "fullscreen" },
  args: {
    cloneId: "pega-encoder-scratch",
    presets: makeClonePresets(),
    preset: "webapp",
    onPresetChange: fn(),
    rebuild: false,
    onRebuildChange: fn(),
    valid: true,
    busy: false,
    error: null,
    operation: null,
    onSubmit: fn(),
    onClose: fn(),
  },
} satisfies Meta<typeof RebaseModalView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** How the dialog opens from the clone menu: the clone's own preset picked, rebuild off. */
export const Default: Story = { args: {} };

/** A different preset picked: the base line follows the pick, Rebase stays live. */
export const OtherPreset: Story = {
  args: { preset: "devtools" },
};

/** Rebuild ticked: the same tag rebuilds fresh (a base release under the same tag does
 *  not invalidate it otherwise). */
export const RebuildChecked: Story = {
  args: { rebuild: true },
};

/** The rebase is running. The form and both buttons lock, and the op's own progress renders
 *  under the fields. */
export const Rebasing: Story = {
  args: {
    busy: true,
    operation: makeOperation({ target: "pega-encoder-scratch" }),
  },
};

/** The start failed. The dialog keeps the whole form so the attempt can be retried as it
 *  stands, and says why in its own footer rather than the page banner. */
export const WithError: Story = {
  args: { error: "preset image build failed: see log" },
};
