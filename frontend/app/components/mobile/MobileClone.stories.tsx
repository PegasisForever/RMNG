import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { MobileClone, type CloneTab } from "./MobileClone";
import { ChatView } from "~/components/ChatView";
import { NotesEditorView } from "~/components/NotesEditorView";
import { PhoneFrame } from "~/stories/PhoneFrame";
import {
  chatActivity,
  chatLocale,
  chatMessages,
  chatNow,
  scheduledMessages,
} from "../__fixtures__/chat";
import { cloneOffline, cloneWorking } from "../__fixtures__/clones";
import { makeNotesBlocks } from "../__fixtures__/notes";

/** The chat pane on fixtures instead of the per-clone SSE stream. */
function ChatFixture({ busy = false }: { busy?: boolean }) {
  const [input, setInput] = useState("");
  const [scheduleAt, setScheduleAt] = useState("");
  const [scheduled, setScheduled] = useState(scheduledMessages);
  return (
    <ChatView
      messages={chatMessages}
      busy={busy}
      activity={busy ? chatActivity : null}
      scheduled={scheduled}
      input={input}
      onInputChange={setInput}
      scheduleAt={scheduleAt}
      onScheduleAtChange={setScheduleAt}
      onSend={fn()}
      onSchedule={fn()}
      onStop={fn()}
      onCancelScheduled={(id) => setScheduled((s) => s.filter((m) => m.id !== id))}
      now={chatNow}
      locale={chatLocale}
    />
  );
}

/** The notes pane on a sample document. Edits go nowhere — no autosave, no upload. */
function NotesFixture() {
  return (
    <NotesEditorView
      initialContent={makeNotesBlocks()}
      onChange={fn()}
      uploadFile={async () => "data:image/gif;base64,R0lGODlhAQABAAAAACw="}
    />
  );
}

const meta = {
  title: "Mobile/Pages/MobileClone",
  component: MobileClone,
  parameters: { layout: "centered" },
  args: {
    clone: cloneWorking,
    tab: "chat" as CloneTab,
    onTabChange: fn(),
    onBack: fn(),
    notes: <NotesFixture />,
    chat: <ChatFixture />,
    error: null,
  },
  /** The page is controlled, so the story holds the selected tab and the switch actually
   *  switches. */
  render: (args) => {
    const [tab, setTab] = useState<CloneTab>(args.tab);
    return (
      <PhoneFrame>
        <MobileClone
          {...args}
          tab={tab}
          onTabChange={(next) => {
            setTab(next);
            args.onTabChange(next);
          }}
        />
      </PhoneFrame>
    );
  },
} satisfies Meta<typeof MobileClone>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The default landing: the agent thread with the composer at the bottom. */
export const Chat: Story = {};

/** Mid-turn. The composer locks, the working bubble appears, and Stop replaces Send. */
export const AgentWorking: Story = {
  args: { chat: <ChatFixture busy /> },
};

/** The notes tab, full screen, on the same block editor the desktop uses. */
export const Notes: Story = {
  args: { tab: "notes" as CloneTab },
};

/** An offline clone still shows its history. The header dot and label are what say so. */
export const Offline: Story = {
  args: { clone: cloneOffline },
};

/** The banner sits between the tabs and the pane, and carries connection failures only.
 *  A failed turn is the chat's own business and renders inside the thread, so the two never
 *  report the same thing twice. */
export const WithError: Story = {
  args: { error: "Failed to fetch" },
};
