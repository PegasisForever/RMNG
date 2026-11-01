import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { chatLocale, chatMessages, chatNow } from "~/components/__fixtures__/chat";
import { cloneIdle, cloneWorking } from "~/components/__fixtures__/clones";
import { ChatView } from "~/components/ChatView";

import { inPageFrame } from "./__fixtures__/frame";
import { NotesView } from "./NotesView";

const meta = {
  title: "ng/Control panel/NotesView",
  component: NotesView,
  decorators: [inPageFrame],
  parameters: { layout: "centered" },
  args: {
    clone: cloneWorking,
    pane: "notes",
    onPaneChange: fn(),
    notesLabel: "Notes",
    notes: (
      <div className="space-y-2 px-4 text-sm text-neutral-700 dark:text-neutral-300">
        <p>The notes editor goes here (it loads in the browser only).</p>
        <p>Plan: normalize the sidebar CPU to a percent of the allowance.</p>
      </div>
    ),
    chat: null,
  },
} satisfies Meta<typeof NotesView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The clone's notes. */
export const Notes: Story = {};

/** The assistant chat. */
export const Assistant: Story = {
  args: {
    pane: "chat",
    chat: (
      <ChatView
        messages={chatMessages}
        input=""
        onInputChange={fn()}
        scheduleAt=""
        onScheduleAtChange={fn()}
        onSend={fn()}
        onSchedule={fn()}
        onStop={fn()}
        onCancelScheduled={fn()}
        now={chatNow}
        locale={chatLocale}
        glass
      />
    ),
  },
};

/** An archived clone's notes, opened from Clones: the way back to the clone on the screen. */
export const ArchivedClone: Story = {
  args: { clone: { ...cloneIdle, archived: true }, onBackToScreen: fn() },
};

/** No clone is selected. */
export const NoClone: Story = { args: { clone: null } };
