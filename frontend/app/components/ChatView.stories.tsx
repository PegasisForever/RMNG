import type { Meta, StoryObj } from "@storybook/react-vite";
import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { ChatView, epochMsToLocalInput, localInputToEpochMs } from "./ChatView";
import {
  chatActivity,
  chatDraft,
  chatError,
  chatLocale,
  chatMessages,
  chatNow,
  makeChatMessage,
  makeScheduledMessage,
  scheduledMessages,
} from "./__fixtures__/chat";

/** The pane fills the chat card in the shell's side column, so the story gives it one of the
 *  same shape rather than letting it size to its contents. The thread scrolls inside it. */
function Frame({ children }: { children: ReactNode }) {
  return (
    <div className="flex h-[38rem] w-[26rem] flex-col overflow-hidden rounded-2xl border border-slate-900/10 bg-white shadow-xl dark:border-white/10 dark:bg-slate-900">
      {children}
    </div>
  );
}

const meta = {
  title: "Clone/Components/ChatView",
  component: ChatView,
  parameters: { layout: "centered" },
  args: {
    messages: chatMessages,
    loading: false,
    busy: false,
    stopping: false,
    activity: null,
    error: null,
    archived: false,
    scheduled: scheduledMessages,
    input: "",
    onInputChange: fn(),
    scheduleAt: "",
    onScheduleAtChange: fn(),
    onSend: fn(),
    onSchedule: fn(),
    onStop: fn(),
    onCancelScheduled: fn(),
    // A fixed instant and a fixed locale, so "Today 15:00" and the picker's floor read the
    // same on every load and on every machine.
    now: chatNow,
    locale: chatLocale,
  },
  render: (args) => (
    <Frame>
      <ChatView {...args} />
    </Frame>
  ),
} satisfies Meta<typeof ChatView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A thread mid-conversation with two messages queued for later, which is the pane at its
 *  fullest: history, the pending queue, and an idle composer. */
export const Populated: Story = {};

/** A clone nobody has talked to yet. The placeholder says what the agent can do, since an
 *  empty thread is the first thing most operators see. */
export const Empty: Story = {
  args: { messages: [], scheduled: [] },
};

/** No snapshot has arrived from the stream yet. Distinct from Empty: the thread is unknown
 *  rather than known to be empty, so the pane says so instead of pitching the agent. */
export const Loading: Story = {
  args: { loading: true, messages: [], scheduled: [] },
};

/** Mid-turn. The working bubble carries the agent's current tool line, the composer locks,
 *  and Stop takes the send button's place. */
export const AgentWorking: Story = {
  args: { busy: true, activity: chatActivity },
};

/** The abort is in flight. The Stop button spins and takes no second click until the stream
 *  reports the turn is over. */
export const Stopping: Story = {
  args: { busy: true, stopping: true, activity: chatActivity, scheduled: [] },
};

/** A send that failed. The banner is the whole of what a send failure can say (the container
 *  drops the server's own words on the floor) and the unsent text is back in the box, ready
 *  for another try. */
export const WithError: Story = {
  args: { error: chatError, input: chatDraft, scheduled: [] },
};

/** An archived clone. The history stays readable, the composer is dead, and a strip above it
 *  says what to do about that. */
export const Archived: Story = {
  args: { archived: true, scheduled: [] },
};

/** The picker open, on the fifteen-minute default the clock button drops in. The submit
 *  button now queues the message instead of sending it. */
export const Scheduling: Story = {
  args: {
    input: chatDraft,
    scheduleAt: epochMsToLocalInput(chatNow + 15 * 60_000),
  },
};

/** The pane wired to local state instead of the stream: sending appends the message and the
 *  agent answers after a beat, Stop cuts the turn short, and the picker really queues. */
export const Interactive: Story = {
  render: function Render(args) {
    const [messages, setMessages] = useState(args.messages);
    const [scheduled, setScheduled] = useState(args.scheduled ?? []);
    const [input, setInput] = useState("");
    const [scheduleAt, setScheduleAt] = useState("");
    const [busy, setBusy] = useState(false);
    // The stand-in for the turn. Held in a ref so Stop can cut it, and dropped on unmount so
    // it cannot land in a story the reader has already left.
    const turn = useRef<ReturnType<typeof setTimeout> | null>(null);
    const endTurn = () => {
      if (turn.current) clearTimeout(turn.current);
      turn.current = null;
    };
    useEffect(() => endTurn, []);

    const send = () => {
      const text = input.trim();
      if (!text || busy) return;
      setInput("");
      setMessages((m) => [...m, makeChatMessage({ id: `u-${m.length}`, text, ts: chatNow })]);
      setBusy(true);
      args.onSend();
      turn.current = setTimeout(() => {
        // One canned reply. The point is the shape of a turn, not the agent.
        setMessages((m) => [
          ...m,
          makeChatMessage({
            id: `a-${m.length}`,
            role: "assistant",
            text: "Done. The screenshot is in the notes.",
            ts: chatNow,
          }),
        ]);
        setBusy(false);
      }, 1400);
    };

    const schedule = () => {
      const text = input.trim();
      const at = localInputToEpochMs(scheduleAt);
      if (!text || at === null) return;
      setScheduled((s) =>
        [...s, makeScheduledMessage({ id: `s-${s.length + 1}`, at: BigInt(at), text })].sort((a, b) =>
          Number(a.at - b.at),
        ),
      );
      setInput("");
      setScheduleAt("");
      args.onSchedule();
    };

    return (
      <Frame>
        <ChatView
          {...args}
          messages={messages}
          busy={busy}
          activity={busy ? chatActivity : null}
          scheduled={scheduled}
          input={input}
          onInputChange={setInput}
          scheduleAt={scheduleAt}
          onScheduleAtChange={setScheduleAt}
          onSend={send}
          onSchedule={schedule}
          onStop={() => {
            endTurn();
            setBusy(false);
            args.onStop();
          }}
          onCancelScheduled={(id) => {
            setScheduled((s) => s.filter((m) => m.id !== id));
            args.onCancelScheduled(id);
          }}
        />
      </Frame>
    );
  },
};
