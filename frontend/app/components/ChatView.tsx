// The chat pane's markup, with no network of its own. Everything it draws arrives as
// props and every intent leaves as a callback, so the same component backs the live
// panel (ChatContainer wires it to the per-clone SSE stream) and the Storybook page.
//
// Two pieces of ambient state it would otherwise read for itself arrive as props instead.
// The clock (`now`) feeds the schedule picker's `min` and the "Today 15:30" labels. The
// locale (`locale`) decides whether that label reads "15:30" or "3:30 PM". Both are required,
// so a story renders identically on every load and on every machine.
//
// It is drawn in glass (`~/desk/GLASS.md`), on a glass panel over the clone's screen: the
// assistant's bubbles are glass of their own (the operator's green glass, the send button's
// green), and the field and the boxes are translucent rather than solid.
import { CalendarClock, Clock, LoaderCircle, SendHorizontal, Square } from "lucide-react";
import { useEffect, useRef } from "react";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

import {
  BUTTON,
  DANGER_BUTTON,
  DONE_BUTTON,
  ErrorLine,
  FIELD,
  ICON_BUTTON,
  LIT,
  R_PIECE,
  TEXT,
  TEXT_2,
  TEXT_3,
} from "~/desk/control/ui";
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

/** "Today 15:30" / "Mon 09:00" / a full date when it's further out, relative to `now`.
 *  `locale` is passed in rather than left to the host default: an ambient default renders
 *  "3:30 PM" on one machine and "15:30" on the next, which a story cannot pin down. */
function formatWhen(ms: number, now: number, locale: string): string {
  const d = new Date(ms);
  const time = d.toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" });
  const today = new Date(now);
  const sameDay = d.toDateString() === today.toDateString();
  if (sameDay) return `Today ${time}`;
  const withinAWeek = ms - now < 7 * 24 * 3600 * 1000;
  if (withinAWeek) return `${d.toLocaleDateString(locale, { weekday: "short" })} ${time}`;
  return `${d.toLocaleDateString(locale, { month: "short", day: "numeric" })} ${time}`;
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
  /** The assistant's current tool line, shown under the working bubble. */
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
  /** Epoch ms treated as "now". Required: the container reads the clock, never this. */
  now: number;
  /** BCP 47 tag the date labels format under ("en-GB" ⇒ "Today 15:00"). Required for the
   *  same reason as `now`: the container resolves the operator's locale, never this. */
  locale: string;
}

/** `~/desk/GLASS.md`: glass bubbles, and its flat content (a field, a button, a box) on the panel. */
const LOOK = {
  // The operator's own: green glass, the send button's green
  user: "glass glass-green",
  assistant: "glass text-neutral-900 dark:text-white",
  working: "glass",
  faint: TEXT_3,
  quiet: TEXT_2,
  line: "border-black/10 dark:border-white/10",
  note: "bg-black/5 text-black/60 dark:bg-white/5 dark:text-white/60",
  item: "border-black/10 bg-white/50 dark:border-white/10 dark:bg-white/5",
  itemText: TEXT_2,
  field: `${FIELD} placeholder:text-black/40 dark:placeholder:text-white/40`,
  button: BUTTON,
  // The schedule button while it is on: the one thing on, flat on the panel
  buttonOn: `${R_PIECE} ${LIT} border-transparent`,
  // Danger is red text, never a red surface. The button's own text color is swapped out, not
  // added to: of two text colors in one class list, the one Tailwind emits last wins.
  stop: DANGER_BUTTON,
  // Send and Schedule end the task: the one green button
  send: `${DONE_BUTTON} border-transparent`,
  // 6 px in from the panel's edge, so its corners are concentric with the panel's (16 − 6)
  composer: "p-1.5",
  radius: R_PIECE,
};

/** A composer button: the height of the one-line field beside it (34 px), so the two line up. */
const SQUARE = "flex size-[34px] shrink-0 items-center justify-center border";

// The assistant answers in markdown. There is no typography plugin in this app, so each element
// gets its own few classes here. Raw HTML in a reply is not rendered (react-markdown's default).
const MARKDOWN: Components = {
  p: ({ children }) => <p className="my-1.5 first:mt-0 last:mb-0">{children}</p>,
  ul: ({ children }) => <ul className="my-1.5 list-disc pl-5">{children}</ul>,
  ol: ({ children }) => <ol className="my-1.5 list-decimal pl-5">{children}</ol>,
  li: ({ children }) => <li className="my-0.5">{children}</li>,
  h1: ({ children }) => <p className="mt-2 mb-1 font-semibold first:mt-0">{children}</p>,
  h2: ({ children }) => <p className="mt-2 mb-1 font-semibold first:mt-0">{children}</p>,
  h3: ({ children }) => <p className="mt-2 mb-1 font-semibold first:mt-0">{children}</p>,
  a: ({ href, children }) => (
    <a href={href} target="_blank" rel="noreferrer" className="text-emerald-700 underline dark:text-emerald-400">
      {children}
    </a>
  ),
  code: ({ children }) => (
    <code className="rounded bg-black/5 px-1 font-mono text-[0.85em] dark:bg-white/10">{children}</code>
  ),
  pre: ({ children }) => (
    <pre className="my-1.5 overflow-x-auto rounded bg-black/5 p-2 font-mono text-xs dark:bg-white/10 [&_code]:bg-transparent [&_code]:p-0">
      {children}
    </pre>
  ),
  blockquote: ({ children }) => (
    <blockquote className="my-1.5 border-l-2 border-black/20 pl-2 text-black/60 dark:border-white/20 dark:text-white/60">
      {children}
    </blockquote>
  ),
  table: ({ children }) => (
    <div className="my-1.5 overflow-x-auto">
      <table className="border-collapse text-xs">{children}</table>
    </div>
  ),
  th: ({ children }) => (
    <th className="border border-black/15 px-1.5 py-0.5 text-left font-semibold dark:border-white/15">{children}</th>
  ),
  td: ({ children }) => <td className="border border-black/15 px-1.5 py-0.5 dark:border-white/15">{children}</td>,
};

/** The server's id for a message sent while the assistant worked, until the assistant takes
 *  it in at its next step. */
function isQueued(m: ChatMessage): boolean {
  return m.id.startsWith("queued-");
}

function Bubble({ m }: { m: ChatMessage }) {
  const isUser = m.role === "user";
  const queued = isQueued(m);
  return (
    <div className={`flex flex-col ${isUser ? "items-end" : "items-start"}`}>
      <div
        className={`max-w-[88%] min-w-0 break-words rounded-2xl px-3 py-2 text-sm ${
          isUser ? `whitespace-pre-wrap ${LOOK.user} ${queued ? "opacity-60" : ""}` : LOOK.assistant
        }`}
      >
        {isUser ? (
          m.text
        ) : (
          <Markdown remarkPlugins={[remarkGfm]} components={MARKDOWN}>
            {m.text}
          </Markdown>
        )}
      </div>
      {queued ? (
        <span className={`mt-0.5 text-xs ${LOOK.faint}`}>
          Queued: the assistant reads it at its next step
        </span>
      ) : null}
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
  now,
  locale,
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
          <p className={`text-sm ${LOOK.faint}`}>Loading…</p>
        ) : messages.length === 0 ? (
          <p className={`text-sm ${LOOK.faint}`}>
            {archived
              ? "This clone is archived. Its chat history is retained."
              : "Ask the assistant anything — it can control this clone's desktop."}
          </p>
        ) : (
          messages.map((m) => <Bubble key={m.id} m={m} />)
        )}
        {busy ? (
          <div className="flex justify-start">
            <div className={`max-w-[88%] rounded-2xl px-3 py-2 text-sm ${LOOK.working}`}>
              <span className={LOOK.faint}>assistant is working…</span>
              {activity ? (
                <span
                  className={`mt-1 block break-words font-mono text-xs leading-snug ${LOOK.quiet}`}
                  data-tip={activity}
                >
                  {activity}
                </span>
              ) : null}
            </div>
          </div>
        ) : null}
      </div>

      {error ? <ErrorLine message={error} /> : null}
      {archived ? (
        <div className={`border-t px-3 py-1.5 text-xs ${LOOK.line} ${LOOK.note}`}>
          Unarchive this clone to message the assistant.
        </div>
      ) : null}

      {scheduled.length > 0 ? (
        <ul className={`max-h-32 space-y-1 overflow-y-auto border-t px-2 py-1.5 ${LOOK.line}`}>
          {scheduled.map((m) => (
            <li
              key={m.id}
              className={`flex items-center gap-2 border px-2 py-1 text-xs ${LOOK.radius} ${LOOK.item}`}
            >
              <span className="shrink-0 font-medium text-emerald-700 dark:text-emerald-400">
                {formatWhen(Number(m.at), now, locale)}
              </span>
              <span className={`min-w-0 flex-1 truncate ${LOOK.itemText}`} data-tip={m.text}>
                {m.text}
              </span>
              <button
                type="button"
                onClick={() => onCancelScheduled(m.id)}
                data-tip="Cancel this scheduled message"
                aria-label="Cancel this scheduled message"
                className={`shrink-0 px-1.5 py-0.5 ${ICON_BUTTON}`}
              >
                ✕
              </button>
            </li>
          ))}
        </ul>
      ) : null}

      <form
        className={`flex flex-col gap-2 border-t ${LOOK.composer} ${LOOK.line}`}
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        {picking ? (
          <div className="flex items-center gap-2">
            <label className={`shrink-0 text-xs ${LOOK.quiet}`} htmlFor="chat-schedule-at">
              Send at
            </label>
            <input
              id="chat-schedule-at"
              type="datetime-local"
              value={scheduleAt}
              min={epochMsToLocalInput(now)}
              onChange={(e) => onScheduleAtChange(e.target.value)}
              className={`min-w-0 flex-1 px-2 py-1 text-sm ${LOOK.field}`}
            />
            <button
              type="button"
              onClick={() => onScheduleAtChange("")}
              className={`shrink-0 px-2 py-1 text-xs ${LOOK.button}`}
            >
              Cancel
            </button>
          </div>
        ) : null}
        {/* The composer starts one line tall, level with the buttons beside it (all 34 px),
            and grows with what is typed up to eight lines before it scrolls. `items-center` keeps
            the buttons in the middle of its height as it grows. Its height is the text's by layout: a
            hidden copy of the text in the same grid cell sizes the cell (no more than eight
            lines), and the field fills it. The copy holds the text, not the placeholder, so
            an empty field is one line on a narrow card too; the placeholder is cut with "…". */}
        <div className="flex items-center gap-2">
          <div className="grid min-w-0 flex-1">
            <div
              aria-hidden
              className="invisible max-h-[174px] overflow-hidden whitespace-pre-wrap break-words border px-2 py-1.5 text-sm leading-5 [grid-area:1/1]"
            >
              {input}{" "}
            </div>
            <textarea
              value={input}
              rows={1}
              onChange={(e) => onInputChange(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  submit();
                }
              }}
              placeholder={
                archived
                  ? "Unarchive to message the assistant"
                  : picking
                    ? "Message to deliver later…  (Enter to schedule)"
                    : busy
                      ? "Steer the assistant while it works…  (Enter to send)"
                      : "Message the assistant…  (Enter to send)"
              }
              // Typing stays enabled while the assistant works: a message then steers it, the
              // way a new Slack message in its thread does.
              disabled={archived}
              className={`min-w-0 resize-none overflow-y-auto whitespace-pre-wrap break-words px-2 py-1.5 text-sm leading-5 [grid-area:1/1] placeholder:truncate disabled:opacity-60 ${LOOK.field}`}
            />
          </div>
          <button
            type="button"
            onClick={() => onScheduleAtChange(picking ? "" : epochMsToLocalInput(now + 15 * 60 * 1000))}
            disabled={archived}
            data-tip={picking ? "Send now instead" : "Schedule this message for later"}
            aria-label={picking ? "Send now instead" : "Schedule this message for later"}
            aria-pressed={picking}
            className={`${SQUARE} disabled:opacity-40 ${picking ? LOOK.buttonOn : LOOK.button}`}
          >
            <Clock className="size-4" />
          </button>
          {picking ? (
            <button
              type="submit"
              disabled={archived || !input.trim() || !scheduleAt}
              data-tip="Schedule this message"
              aria-label="Schedule this message"
              className={`${SQUARE} ${LOOK.send}`}
            >
              <CalendarClock className="size-4" />
            </button>
          ) : (
            <>
              {busy && !archived ? (
                <button
                  type="button"
                  onClick={onStop}
                  disabled={stopping}
                  data-tip={stopping ? "Stopping…" : "Interrupt the assistant's current turn"}
                  aria-label={stopping ? "Stopping…" : "Interrupt the assistant's current turn"}
                  className={`${SQUARE} disabled:opacity-50 ${LOOK.stop}`}
                >
                  {/* The spinner is the only way an icon-only button can say the stop is in
                      flight, now that "Stopping…" is not there to read. */}
                  {stopping ? (
                    <LoaderCircle className="size-4 animate-spin" />
                  ) : (
                    <Square className="size-4 fill-current" />
                  )}
                </button>
              ) : null}
              <button
                type="submit"
                disabled={archived || !input.trim()}
                data-tip={busy ? "Send now: the assistant reads it at its next step" : "Send this message"}
                aria-label={busy ? "Send now: the assistant reads it at its next step" : "Send this message"}
                className={`${SQUARE} ${LOOK.send}`}
              >
                <SendHorizontal className="size-4" />
              </button>
            </>
          )}
        </div>
      </form>
    </div>
  );
}
