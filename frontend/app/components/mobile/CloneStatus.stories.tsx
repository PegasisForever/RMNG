import type { Meta, StoryObj } from "@storybook/react-vite";

import { CloneStatusDot, statusLabel } from "./CloneStatus";
import type { Clone } from "~/lib/types";
import { makeClone } from "../__fixtures__/clones";

/** The dot is 10px, so the story sets it beside the word it stands for — which is also how
 *  the clone screen's header draws it. */
function Frame({ clone }: { clone: Clone }) {
  return (
    <p className="flex w-64 items-center gap-1.5 text-xs text-slate-500 dark:text-slate-400">
      <CloneStatusDot clone={clone} />
      <span className="truncate">{statusLabel(clone)}</span>
    </p>
  );
}

const meta = {
  title: "Mobile/Components/CloneStatusDot",
  component: CloneStatusDot,
  parameters: { layout: "centered" },
  args: { clone: makeClone({ monitorState: "working" }) },
  render: (args) => <Frame clone={args.clone} />,
} satisfies Meta<typeof CloneStatusDot>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Blue: the agent has produced tokens recently. */
export const Working: Story = {};

/** Gray: the container is up and the agent is waiting on somebody. */
export const Idle: Story = { args: { clone: makeClone({ monitorState: "idle" }) } };

/** Purple: the clone's wrapper is unreachable, so nothing can be asked of it. */
export const Offline: Story = { args: { clone: makeClone({ monitorState: "offline" }) } };

/** Red: it stopped working and nobody has looked since. The badge outranks the state dot,
 *  which is what makes one unread clone findable in a long list. */
export const Unread: Story = {
  args: { clone: makeClone({ monitorState: "idle", unread: true }) },
};

/** Archived, which outranks the unread badge: a clone stopped on purpose is not news. */
export const Archived: Story = {
  args: { clone: makeClone({ monitorState: "idle", archived: true, unread: true }) },
};

/** A clone the server has said nothing about yet. No `monitorState` reads as not working,
 *  rather than as an error. */
export const NoState: Story = { args: { clone: makeClone({ monitorState: undefined }) } };
