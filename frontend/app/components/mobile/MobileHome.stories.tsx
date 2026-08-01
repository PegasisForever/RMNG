import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { MobileHome } from "./MobileHome";
import { PhoneFrame } from "~/stories/PhoneFrame";
import {
  accountsNow,
  makeClaudeAccounts,
  makeCloneGroups,
  makeCodexGroups,
} from "../__fixtures__/accounts";
import { makeBoardColumns } from "../__fixtures__/board";
import { hosts } from "../__fixtures__/clones";
import { makeStoryLink } from "../__fixtures__/storyLinks";

/** Everything the page is handed that something downstream holds, rebuilt per story: the
 *  usage panel copies the account rows and the pools, and the columns are a list the board
 *  rewrites on the desktop. One set behind every story is how an edit in one leaks into the
 *  next.
 *
 *  These props are deliberately absent from `meta.args`, so TypeScript refuses a story that
 *  does not call this. A default on the meta would be one set of objects built once at module
 *  scope, which is the leak this exists to prevent, and it would go unnoticed because the
 *  story would still compile and still render. */
function base() {
  return {
    accounts: makeClaudeAccounts(accountsNow),
    // The dashboard container subscribes to the shared order store and passes the value.
    // Nothing has been dragged in a story, so the rows keep the order they arrive in.
    accountOrder: {},
    cloneGroups: makeCloneGroups(),
    codexGroups: makeCodexGroups(),
    // The container's clock, pinned to the instant the account fixtures are written for, so
    // each bar's pace marker and reset countdown read the same on every machine.
    now: accountsNow,
    columns: makeBoardColumns(),
    clones: hosts,
  };
}

const meta = {
  title: "Mobile/Pages/MobileHome",
  component: MobileHome,
  parameters: { layout: "centered" },
  args: {
    locale: "en-GB",
    usageOpen: false,
    onUsageOpenChange: fn(),
    onRefresh: fn(),
    // Both of these leave this screen, so they jump to what they open rather than doing it
    // here: tapping a row is the phone's whole navigation, and the import dialog is mounted
    // by the container beside this page, never inside it.
    onImportAccount: makeStoryLink("Settings/Components/ImportAccountModalView", "SignedIn"),
    onSelectClone: makeStoryLink("Mobile/Pages/MobileClone", "Chat"),
    error: null,
  },
  render: (args) => (
    <PhoneFrame>
      <MobileHome {...args} />
    </PhoneFrame>
  ),
} satisfies Meta<typeof MobileHome>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The usual view: usage collapsed to its peak number, then the clones under their board
 *  columns. The headers stick as you scroll, and an empty column is dropped entirely (the
 *  Archived column here holds nothing, so it never appears). */
export const Default: Story = { args: { ...base() } };

/** Nothing filed yet. With no columns stored, the same defaults the desktop board uses
 *  apply, so every clone lands under one "Clones" header. */
export const NoColumns: Story = {
  args: { ...base(), columns: [] },
};

/** Usage expanded, which is how you check a pool before starting something heavy. It costs
 *  most of the screen, which is why it is not the default. */
export const UsageOpen: Story = {
  args: { ...base(), usageOpen: true },
};

/** A fresh install: accounts imported but nothing cloned yet. The empty state points at the
 *  desktop, because a phone cannot create a clone. */
export const NoClones: Story = {
  args: { ...base(), clones: [] },
};

/** Nothing imported and nothing cloned, so both sections have to say so. */
export const NothingYet: Story = {
  args: { ...base(), accounts: [], cloneGroups: [], codexGroups: [], columns: [], clones: [] },
};

/** The banner, carrying the failure a phone actually hits: the control server went out of
 *  reach. "Failed to fetch" is the browser's own message for a thrown request, and it is
 *  what every call here surfaces once the LAN drops or the phone leaves wifi.
 *
 *  The page's other sources are the two buttons inside the usage panel, refresh and import.
 *  Nothing else on this screen calls the server, so nothing else can fill this banner. */
export const WithError: Story = {
  args: { ...base(), error: "Failed to fetch" },
};

/** The page wired to local state instead of the container, so the usage section really folds
 *  and unfolds. Tapping a clone still leaves for its screen: that is navigation either way. */
export const Interactive: Story = {
  args: { ...base() },
  render: function Render(args) {
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
};
