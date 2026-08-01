import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { MobileHome } from "./MobileHome";
import { PhoneFrame } from "~/stories/PhoneFrame";
import { claudeAccounts, cloneGroups, codexGroups } from "../__fixtures__/accounts";
import { boardColumns } from "../__fixtures__/board";
import { hosts } from "../__fixtures__/clones";

const meta = {
  title: "Page/Mobile/Home",
  component: MobileHome,
  parameters: { layout: "centered" },
  args: {
    accounts: claudeAccounts,
    cloneGroups,
    codexGroups,
    usageOpen: false,
    onUsageOpenChange: fn(),
    onRefresh: fn(),
    onImportAccount: fn(),
    columns: boardColumns,
    clones: hosts,
    onSelectClone: fn(),
    error: null,
  },
  /** The page is controlled, so the story owns the one piece of state it has: whether the
   *  usage section is expanded. */
  render: (args) => {
    const [usageOpen, setUsageOpen] = useState(args.usageOpen);
    return (
      <PhoneFrame>
        <MobileHome
          {...args}
          usageOpen={usageOpen}
          onUsageOpenChange={(open) => {
            setUsageOpen(open);
            args.onUsageOpenChange(open);
          }}
        />
      </PhoneFrame>
    );
  },
} satisfies Meta<typeof MobileHome>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The usual view: usage collapsed to its peak number, then the clones under their board
 *  columns. The headers stick as you scroll, and an empty column is dropped entirely (the
 *  Archived column here holds nothing, so it never appears). */
export const Default: Story = {};

/** Nothing filed yet. With no columns stored, the same defaults the desktop board uses
 *  apply, so every clone lands under one "Clones" header. */
export const NoColumns: Story = {
  args: { columns: [] },
};

/** Usage expanded, which is how you check a pool before starting something heavy. It costs
 *  most of the screen, which is why it is not the default. */
export const UsageOpen: Story = {
  args: { usageOpen: true },
};

/** A fresh install: accounts imported but nothing cloned yet. The empty state points at the
 *  desktop, because a phone cannot create a clone. */
export const NoClones: Story = {
  args: { clones: [] },
};

/** Nothing imported and nothing cloned, so both sections have to say so. */
export const NothingYet: Story = {
  args: { accounts: [], cloneGroups: [], codexGroups: [], columns: [], clones: [] },
};

/** The banner, carrying the failure a phone actually hits: the control server went out of
 *  reach. "Failed to fetch" is the browser's own message for a thrown request, and it is
 *  what every call here surfaces once the LAN drops or the phone leaves wifi.
 *
 *  The page's other sources are the two buttons inside the usage panel, refresh and import.
 *  Nothing else on this screen calls the server, so nothing else can fill this banner. */
export const WithError: Story = {
  args: { error: "Failed to fetch" },
};
