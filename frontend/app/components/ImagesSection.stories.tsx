import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { ImagesSection } from "./ImagesSection";
import { imagesNow, makeImage, makeImages } from "./__fixtures__/images";

/** The section sits inside the settings panel's column, so the story gives it that width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[36rem] p-4">{children}</div>;
}

const meta = {
  title: "Settings/Components/ImagesSection",
  component: ImagesSection,
  parameters: { layout: "centered" },
  args: {
    // Rebuilt per story below: the list is what a delete takes a row out of.
    images: makeImages(),
    loading: false,
    pullBusy: false,
    templateRef: "pegasis0/rmng-template:latest",
    // The container's clock, pinned to the instant the image fixtures are written for, so
    // every row's age reads the same on every machine.
    now: imagesNow,
    onPullLatest: fn(),
    onPullOther: fn(),
    onDelete: fn(),
  },
  render: (args) => (
    <Frame>
      <ImagesSection {...args} />
    </Frame>
  ),
} satisfies Meta<typeof ImagesSection>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The usual list: the wizard-pulled template carrying its "base" badge and two live clones,
 *  and a committed image nothing is running on. Only the second one can be deleted. */
export const Default: Story = { args: { images: makeImages() } };

/** The first fetch is still out. One line stands in for the list, and the two pull actions
 *  stay live: neither of them needs the list to exist. */
export const Loading: Story = {
  args: { images: [], loading: true },
};

/** A fresh install with nothing pulled. The empty state says what to do about it. */
export const Empty: Story = {
  args: { images: [] },
};

/** A template pull is already running, so both pull buttons are dead until it settles. */
export const Pulling: Story = {
  args: { images: makeImages(), pullBusy: true },
};

/** Every image is in use, so every delete is blocked. The row's own title says by how many
 *  clones, because that is the thing to go and stop first. */
export const AllInUse: Story = {
  args: {
    images: [
      makeImage({ base: true, inUseBy: ["pega-we-142"] }),
      makeImage({
        id: "sha256:bbbb1111",
        reference: "node20:latest",
        sizeBytes: BigInt(7_200_000_000),
        createdAt: "2026-06-28T09:30:00Z",
        inUseBy: ["pega-dev-88", "pega-hh-7"],
      }),
    ],
  },
};

/** No template reference configured, which is what leaves the "Pull latest" button naming the
 *  configured reference in the abstract. */
export const NoTemplateReference: Story = {
  args: { images: makeImages(), templateRef: "" },
};

/** The section wired to local state instead of the container: a delete really takes its row
 *  out of the list, and a pull locks both buttons for a beat the way a running op does. */
export const Interactive: Story = {
  args: { images: makeImages() },
  render: function Render(args) {
    const [images, setImages] = useState(args.images);
    const [pullBusy, setPullBusy] = useState(args.pullBusy);
    // The point is the shape of a pull, not the registry: lock the buttons, then let go.
    const pull = () => {
      setPullBusy(true);
      window.setTimeout(() => setPullBusy(false), 1200);
    };
    return (
      <Frame>
        <ImagesSection
          {...args}
          images={images}
          pullBusy={pullBusy}
          onPullLatest={() => {
            pull();
            args.onPullLatest();
          }}
          onPullOther={() => {
            pull();
            args.onPullOther();
          }}
          onDelete={(reference) => {
            setImages((prev) => prev.filter((i) => i.reference !== reference));
            args.onDelete(reference);
          }}
        />
      </Frame>
    );
  },
};
