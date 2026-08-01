import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";

import { ImagePicker } from "./ImagePicker";
import { imagesNow, makeImages } from "./__fixtures__/images";
import { preferredCloneImage } from "~/lib/lastCloneImage";

const meta = {
  title: "Clone/Components/ImagePicker",
  component: ImagePicker,
  parameters: { layout: "centered" },
  // Local state so the dropdown actually selects.
  render: (args) => {
    const [value, setValue] = useState<string | null>(args.value);
    return (
      <div className="w-80 text-xs font-medium text-slate-500 dark:text-slate-400">
        Source image
        <ImagePicker {...args} value={value} onChange={setValue} />
        <p className="mt-2 text-[11px] font-normal text-slate-400 dark:text-slate-500">
          selected: <span className="font-mono">{value ?? "(none)"}</span>
        </p>
      </div>
    );
  },
  args: {
    images: makeImages(),
    loading: false,
    value: null,
    now: imagesNow,
    onChange: () => {},
  },
} satisfies Meta<typeof ImagePicker>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The clone-source template picker — a dropdown of the available images, the newest one
 *  preselected.
 *
 *  Which image a fresh dialog starts on arrives as a prop rather than being worked out here:
 *  the rule reads the last-cloned-from reference out of localStorage, so the clone dialog's
 *  container owns it. The story derives the same answer from the same function, with nothing
 *  remembered. */
export const Default: Story = {
  args: { value: preferredCloneImage(makeImages(), null) },
};

/** Still fetching the image list. */
export const Loading: Story = {
  args: { images: [], loading: true },
};

/** No base image built yet — the operator must build one first. */
export const Empty: Story = {
  args: { images: [], loading: false },
};
