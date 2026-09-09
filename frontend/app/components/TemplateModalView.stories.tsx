import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { TemplateModalView } from "./TemplateModalView";
import { makeOperation } from "./__fixtures__/operations";
import { makeClonePresets } from "./__fixtures__/presets";

const meta = {
  title: "Clone/Components/TemplateModalView",
  component: TemplateModalView,
  parameters: { layout: "fullscreen" },
  args: {
    title: "",
    onTitleChange: fn(),
    presets: makeClonePresets(),
    preset: "webapp",
    onPresetChange: fn(),
    valid: false,
    busy: false,
    error: null,
    operation: null,
    onSubmit: fn(),
    onClose: fn(),
  },
} satisfies Meta<typeof TemplateModalView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** How the dialog opens: empty title, first preset picked, Clone dead. */
export const Default: Story = { args: {} };

/** Title typed: Clone is live. */
export const Ready: Story = {
  args: { title: "encoder-scratch", valid: true },
};

/** No presets configured: the dropdown explains itself, Clone still fires on title. */
export const NoPresets: Story = {
  args: { presets: [], preset: "", title: "encoder-scratch", valid: true },
};

/** The clone is running. The form and both buttons lock, and the op's own progress renders
 *  under the fields. */
export const Cloning: Story = {
  args: {
    title: "encoder-scratch",
    valid: true,
    busy: true,
    operation: makeOperation({ target: "pega-encoder-scratch" }),
  },
};

/** The start failed. The dialog keeps the whole form so the attempt can be retried as it
 *  stands, and says why in its own footer rather than the page banner. */
export const WithError: Story = {
  args: {
    title: "encoder-scratch",
    valid: true,
    error: "template pull failed: network unreachable",
  },
};
