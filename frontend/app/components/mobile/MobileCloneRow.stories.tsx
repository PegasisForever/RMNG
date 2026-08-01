import type { Meta, StoryObj } from "@storybook/react-vite";

import { MobileCloneRow } from "./MobileCloneRow";
import {
  cloneIdle,
  cloneUnmanaged,
  cloneWorking,
  makeCloneIdle,
  makeCloneOffline,
  makeCloneWorking,
} from "../__fixtures__/clones";
import { makeStoryLink } from "../__fixtures__/storyLinks";

/** The row is a list item, so the story gives it the list it lives in, at a phone's width. */
function Frame({ children }: { children: React.ReactNode }) {
  return (
    <ul className="w-[390px] divide-y divide-slate-200 bg-white dark:divide-slate-800 dark:bg-slate-900">
      {children}
    </ul>
  );
}

const meta = {
  title: "Mobile/Components/MobileCloneRow",
  component: MobileCloneRow,
  parameters: { layout: "centered" },
  args: {
    clone: cloneWorking,
    // Tapping a row opens that clone's screen, which is the phone's whole navigation.
    onSelect: makeStoryLink("Mobile/Pages/MobileClone", "Chat"),
  },
  render: (args) => (
    <Frame>
      <MobileCloneRow {...args} />
    </Frame>
  ),
} satisfies Meta<typeof MobileCloneRow>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Working, on a ticket, pinned to an account: the blue dot and both halves of the subtitle. */
export const Working: Story = {};

/** Running, with nothing in flight. The dot goes gray and the row reads the same otherwise. */
export const Idle: Story = { args: { clone: makeCloneIdle({ unread: false }) } };

/** It stopped working while nobody was looking. The red badge replaces the dot, and it is the
 *  one thing on this screen worth a second colour. */
export const Unread: Story = { args: { clone: cloneIdle } };

/** The wrapper is unreachable, so the row is purple. Its notes and its thread are still
 *  there to read. */
export const Offline: Story = { args: { clone: makeCloneOffline({ monitorState: "offline" }) } };

/** Archived. The dot goes quiet even with an unread flag standing, because an archived clone
 *  stopped on purpose. */
export const Archived: Story = {
  args: { clone: makeCloneWorking({ archived: true, unread: true }) },
};

/** An unmanaged clone: no ticket and no account, so the subtitle falls back to the id, which
 *  is then the only thing telling two of them apart. */
export const NoSubtitle: Story = { args: { clone: cloneUnmanaged } };

/** A name longer than the row. Both lines truncate rather than wrap, so every row keeps the
 *  same height and the list scrolls at a predictable pace. */
export const LongName: Story = {
  args: {
    clone: makeCloneWorking({
      displayName:
        "Normalize the sidebar CPU readout to a percentage of the clone's own allowance",
      claudeAccountEmail: "a-very-long-account-address@some-long-domain.example.com",
    }),
  },
};
