// The chat pane's markup, with no network of its own. Everything it draws arrives as
// props and every intent leaves as a callback, so the same component backs the live
// panel (ChatPanel wires it to the per-clone SSE stream) and the Storybook page.
//
// The one piece of ambient state it needs is the clock: the schedule picker's `min` and
// the "Today 15:30" labels both read the current time. That comes in as `now` so a story
// renders identically on every load.
import { CalendarClock, Clock, LoaderCircle, SendHorizontal, Square } from "lucide-react";
import { useEffect, useRef } from "react";
import TextareaAutosize from "react-textarea-autosize";

import type { ChatMessage } from "~/lib/types";
import type { ScheduledMessage } from "~/lib/wire/ScheduledMessage";

// `<input type="datetime-local">` speaks naive local wall-clock ("2026-07-27T15:30"), while
// the wire is absolute epoch ms. These two convert across that boundary; `Date`'s local-time
// constructor/getters do the timezone work, so a picked time means what the operator's clock
// shows regardless of where the server is.
export function localInputToEpochMs(v: string): number | null {
  const ms = new Date(v).getTime();
  return Number.isFinite(ms) ? ms : null;
}

export function epochMsToLocalInput(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** "Today 15:30" / "Mon 09:00" / a full date when it's further out, relative to `now`. */
export function formatWhen(ms: number, now: number): string {
  const d = new Date(ms);
  const time = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  const today = new Date(now);
  const sameDay = d.toDateString() === today.toDateString();
  if (sameDay) return `Today ${time}`;
  const withinAWeek = ms - now < 7 * 24 * 3600 * 1000;
  if (withinAWeek) return `${d.toLocaleDateString(undefined, { weekday: "short" })} ${time}`;
  return `${d.toLocaleDateString(undefined, { month: "short", day: "numeric" })} ${time}`;
}

export interface ChatViewProps {
  /** The thread, oldest first. */
  messages: ChatMessage[];
  /** No snapshot has arrived yet — show the placeholder instead of an empty thread. */
  loading?: boolean;
  /** A turn is in flight: the composer locks and the working bubble appears. */
  busy?: boolean;
  /** An abort is in flight (the Stop button's own pending state). */
  stopping?: boolean;
  /** The agent's current tool line, shown under the working bubble. */
  activity?: string | null;
  error?: string | null;
  /** An archived clone keeps its history but takes no new messages. */
  archived?: boolean;
  /** Messages queued for later delivery, soonest first. */
  scheduled?: ScheduledMessage[];
  /** Composer text (controlled). */
  input: string;
  onInputChange: (text: string) => void;
  /** The picker's `datetime-local` value. Non-empty means the composer schedules
   *  instead of sending, so there is no second flag that can drift out of sync. */
  scheduleAt: string;
  onScheduleAtChange: (value: string) => void;
  onSend: () => void;
  onSchedule: () => void;
  onStop: () => void;
  onCancelScheduled: (id: string) => void;
  /** Epoch ms treated as "now". Defaults to the real clock. */
  now?: number;
}

function Bubble({ m }: { m: ChatMessage }) {
  const isUser = m.role === "user";
  return (
    <div className={`flex ${isUser ? "justify-end" : "justify-start"}`}>
      <div
        className={`max-w-[88%] whitespace-pre-wrap rounded-2xl px-3 py-2 text-sm ${
          isUser
            ? "bg-emerald-600 text-white"
            : "border border-slate-200 bg-white text-slate-800 dark:border-slate-700 dark:bg-slate-800 dark:text-slate-100"
        }`}
      >
        {m.text}
      </div>
    </div>
  );
}

export function ChatView({
  messages,
  loading = false,
  busy = false,
  stopping = false,
  activity = null,
  error = null,
  archived = false,
  scheduled = [],
  input,
  onInputChange,
  scheduleAt,
  onScheduleAtChange,
  onSend,
  onSchedule,
  onStop,
  onCancelScheduled,
  now = Date.now(),
}: ChatViewProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const picking = scheduleAt !== "";

  // Pin the thread to the bottom as it grows. Scoped to this component's own node.
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, busy]);

  const submit = () => {
    if (picking) onSchedule();
    else onSend();
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div
        ref={scrollRef}
        className="min-h-0 flex-1 space-y-3 overflow-y-auto px-3 py-3"
      >
        {loading ? (
          <p className="text-sm text-slate-400 dark:text-slate-500">Loading…</p>
        ) : messages.length === 0 ? (
          <p className="text-sm text-slate-400 dark:text-slate-500">
            {archived
              ? "This clone is archived. Its chat history is retained."
              : "Ask the agent anything — it can control this clone's desktop."}
          </p>
        ) : (
          messages.map((m) => <Bubble key={m.id} m={m} />)
        )}
        {busy ? (
          <div className="flex justify-start">
            <div className="max-w-[88%] rounded-2xl border border-slate-200 bg-white px-3 py-2 text-sm dark:border-slate-700 dark:bg-slate-800">
              <span className="text-slate-400 dark:text-slate-500">agent is working…</span>
              {activity ? (
                <span
                  className="mt-1 block break-words font-mono text-xs leading-snug text-slate-500 dark:text-slate-400"
                  title={activity}
                >
                  {activity}
                </span>
              ) : null}
            </div>
          </div>
        ) : null}
      </div>

      {error ? (
        <div className="border-t border-red-200 bg-red-50 px-3 py-1.5 text-xs text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-400">
          {error}
        </div>
      ) : null}
      {archived ? (
        <div className="border-t border-slate-200 bg-slate-100 px-3 py-1.5 text-xs text-slate-500 dark:border-slate-700 dark:bg-slate-800 dark:text-slate-400">
          Unarchive this clone to message the agent.
        </div>
      ) : null}

      {scheduled.length > 0 ? (
        <ul className="max-h-32 space-y-1 overflow-y-auto border-t border-slate-200 px-2 py-1.5 dark:border-slate-700">
          {scheduled.map((m) => (
            <li
              key={m.id}
              className="flex items-center gap-2 rounded-md border border-slate-300 bg-slate-50 px-2 py-1 text-xs dark:border-slate-600 dark:bg-slate-800"
            >
              <span className="shrink-0 font-medium text-emerald-700 dark:text-emerald-400">
                {formatWhen(Number(m.at), now)}
              </span>
              <span className="min-w-0 flex-1 truncate text-slate-600 dark:text-slate-300" title={m.text}>
                {m.text}
              </span>
              <button
                type="button"
                onClick={() => onCancelScheduled(m.id)}
                title="Cancel this scheduled message"
                className="shrink-0 rounded-md px-1.5 py-0.5 text-slate-400 hover:bg-slate-200 hover:text-red-600 dark:hover:bg-slate-700 dark:hover:text-red-400"
              >
                ✕
              </button>
            </li>
          ))}
        </ul>
      ) : null}

      <form
        className="flex flex-col gap-2 border-t border-slate-200 p-2 dark:border-slate-700"
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        {picking ? (
          <div className="flex items-center gap-2">
            <label className="shrink-0 text-xs text-slate-500 dark:text-slate-400" htmlFor="chat-schedule-at">
              Send at
            </label>
            <input
              id="chat-schedule-at"
              type="datetime-local"
              value={scheduleAt}
              min={epochMsToLocalInput(now)}
              onChange={(e) => onScheduleAtChange(e.target.value)}
              className="min-w-0 flex-1 rounded-md border border-slate-300 bg-white px-2 py-1 text-sm text-slate-900 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100"
            />
            <button
              type="button"
              onClick={() => onScheduleAtChange("")}
              className="shrink-0 rounded-md border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-100 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-700"
            >
              Cancel
            </button>
          </div>
        ) : null}
        {/* The composer starts one line tall, level with the buttons beside it, and grows
            with what is typed up to eight lines before it scrolls. `items-end` keeps the
            buttons on the bottom line as it grows. `TextareaAutosize` measures the content
            against a hidden copy of the field, which is the only way to get the height
            right for wrapped text without reading it back from a layout pass. */}
        <div className="flex items-end gap-2">
          <TextareaAutosize
            value={input}
            onChange={(e) => onInputChange(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                submit();
              }
            }}
            minRows={1}
            maxRows={8}
            placeholder={
              archived
                ? "Unarchive to message the agent"
                : picking
                  ? "Message to deliver later…  (Enter to schedule)"
                  : "Message the agent…  (Enter to send)"
            }
            // Typing stays enabled while the picker is open even mid-turn: scheduling *during*
            // a busy turn is the case the feature exists for.
            disabled={archived || (busy && !picking)}
            className="min-w-0 flex-1 resize-none rounded-md border border-slate-300 bg-white px-2 py-1.5 text-sm text-slate-900 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none disabled:opacity-60 dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
          />
          <button
            type="button"
            onClick={() => onScheduleAtChange(picking ? "" : epochMsToLocalInput(now + 15 * 60 * 1000))}
            disabled={archived}
            title={picking ? "Send now instead" : "Schedule this message for later"}
            aria-label={picking ? "Send now instead" : "Schedule this message for later"}
            aria-pressed={picking}
            className={`shrink-0 rounded-md border p-2 disabled:opacity-40 ${
              picking
                ? "border-emerald-600 bg-emerald-50 text-emerald-700 dark:border-emerald-500 dark:bg-emerald-950/40 dark:text-emerald-400"
                : "border-slate-300 text-slate-500 hover:bg-slate-100 dark:border-slate-600 dark:text-slate-400 dark:hover:bg-slate-700"
            }`}
          >
            <Clock className="size-4" />
          </button>
          {picking ? (
            <button
              type="submit"
              disabled={archived || !input.trim() || !scheduleAt}
              title="Schedule this message"
              aria-label="Schedule this message"
              className="shrink-0 rounded-md bg-emerald-600 p-2 text-white hover:bg-emerald-700 disabled:opacity-40"
            >
              <CalendarClock className="size-4" />
            </button>
          ) : busy && !archived ? (
            <button
              type="button"
              onClick={onStop}
              disabled={stopping}
              title={stopping ? "Stopping…" : "Interrupt the agent's current turn"}
              aria-label={stopping ? "Stopping…" : "Interrupt the agent's current turn"}
              className="shrink-0 rounded-md bg-red-600 p-2 text-white hover:bg-red-700 disabled:opacity-50"
            >
              {/* The spinner is the only way an icon-only button can say the stop is in
                  flight, now that "Stopping…" is not there to read. */}
              {stopping ? (
                <LoaderCircle className="size-4 animate-spin" />
              ) : (
                <Square className="size-4 fill-current" />
              )}
            </button>
          ) : (
            <button
              type="submit"
              disabled={archived || !input.trim()}
              title="Send this message"
              aria-label="Send this message"
              className="shrink-0 rounded-md bg-emerald-600 p-2 text-white hover:bg-emerald-700 disabled:opacity-40"
            >
              <SendHorizontal className="size-4" />
            </button>
          )}
        </div>
      </form>
    </div>
  );
}

export default ChatView;
