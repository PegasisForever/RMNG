// Per-clone chat with the in-container agent (Claude Agent SDK). Client-only, lazy-imported
// and keyed by clone id (same pattern as CloneEditor). Subscribes to the per-clone
// chat SSE (/api/chat/:id/events) for { busy, messages, scheduled }, so the agent's reply
// and the "working" indicator survive a refresh — the POST only kicks the turn
// off; the reply lands over SSE. Posting a message is fire-and-forget.
//
// Messages can also be *scheduled*: the operator picks a wall-clock time and the server
// delivers it later from a disk-backed queue (surviving a closed browser and a control-server
// restart). The pending queue rides the same SSE frame, so a cancel in one tab is reflected
// in every other one with no extra stream to manage here.
import { useEffect, useRef, useState } from "react";

import type { ChatMessage } from "~/lib/types";
import type { ScheduledMessage } from "~/lib/wire/ScheduledMessage";

interface ChatSnapshot {
  busy: boolean;
  activity?: string;
  messages: ChatMessage[];
  scheduled?: ScheduledMessage[];
}

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

/** "Today 15:30" / "Mon 09:00" / a full date when it's further out. */
function formatWhen(ms: number): string {
  const d = new Date(ms);
  const time = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  const today = new Date();
  const sameDay = d.toDateString() === today.toDateString();
  if (sameDay) return `Today ${time}`;
  const withinAWeek = ms - today.getTime() < 7 * 24 * 3600 * 1000;
  if (withinAWeek) return `${d.toLocaleDateString(undefined, { weekday: "short" })} ${time}`;
  return `${d.toLocaleDateString(undefined, { month: "short", day: "numeric" })} ${time}`;
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

export default function ChatPanel({ cloneId, archived = false }: { cloneId: string; archived?: boolean }) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [activity, setActivity] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [scheduled, setScheduled] = useState<ScheduledMessage[]>([]);
  const [scheduleAt, setScheduleAt] = useState(""); // "" ⇒ the picker is closed
  const scrollRef = useRef<HTMLDivElement>(null);
  // The picker doubles as the mode switch: while a time is held, the composer schedules
  // instead of sending, so there is no second boolean that can drift out of sync with it.
  const picking = scheduleAt !== "";

  // Live thread + turn state over SSE. EventSource auto-reconnects, so a refresh
  // (or a transient drop) re-syncs from the server's snapshot — including an
  // in-flight turn started by a previous page load.
  useEffect(() => {
    setLoading(true);
    setError(null);
    const es = new EventSource(`/api/chat/${cloneId}/events`);
    es.onmessage = (e) => {
      try {
        const snap = JSON.parse(e.data) as ChatSnapshot;
        setMessages(Array.isArray(snap.messages) ? snap.messages : []);
        setBusy(!!snap.busy);
        if (!snap.busy) setStopping(false); // turn ended — reset the Stop button
        setActivity(snap.activity ?? null);
        setScheduled(Array.isArray(snap.scheduled) ? snap.scheduled : []);
        setLoading(false);
      } catch {
        // ignore malformed frame
      }
    };
    return () => es.close();
  }, [cloneId]);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, busy]);

  // Fire-and-forget: the POST only starts the turn; the reply and final busy state arrive
  // through SSE.
  async function send() {
    const text = input.trim();
    if (!text || busy || archived) return;
    setInput("");
    setError(null);
    setBusy(true); // optimistic; the SSE snapshot confirms (or clears) it
    setActivity(null);
    // Optimistic user bubble; the server snapshot replaces it once it arrives.
    setMessages((m) => [...m, { id: `tmp-${Date.now()}`, role: "user", text, ts: Date.now() }]);
    try {
      const res = await fetch(`/api/chat/${cloneId}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ text }),
      });
      if (!res.ok) {
        const data = (await res.json().catch(() => ({}))) as { error?: string };
        throw new Error(data.error ?? "chat failed");
      }
      // Success: nothing to do — the SSE stream delivers the authoritative
      // messages and busy state from here.
    } catch (e) {
      setError((e as Error).message);
      setInput(text); // restore the unsent text
      setBusy(false); // the turn never started; SSE will reconcile messages
    }
  }

  // Queue the composer's text for later. Unlike `send`, this is allowed while the agent is
  // busy — deferring a message past an in-flight turn is exactly what scheduling is for. The
  // server echoes the new queue over SSE, so there is no optimistic entry to reconcile.
  async function schedule() {
    const text = input.trim();
    const at = localInputToEpochMs(scheduleAt);
    if (!text || archived || at === null) return;
    if (at <= Date.now()) {
      setError("Pick a time in the future.");
      return;
    }
    setError(null);
    try {
      const res = await fetch(`/api/chat/${cloneId}/schedule`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ text, at }),
      });
      if (!res.ok) {
        // The chat routes answer errors as a plain-text body (axum `(StatusCode, String)`).
        const body = (await res.text().catch(() => "")).trim();
        throw new Error(body || "schedule failed");
      }
      setInput("");
      setScheduleAt(""); // collapse the picker back down
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function cancelScheduled(sid: string) {
    setError(null);
    try {
      const res = await fetch(`/api/chat/${cloneId}/schedule/${sid}`, { method: "DELETE" });
      if (!res.ok && res.status !== 404) throw new Error("cancel failed");
      // 404 means it already fired or another tab cancelled it — SSE has the truth either way.
      setScheduled((s) => s.filter((m) => m.id !== sid));
    } catch (e) {
      setError((e as Error).message);
    }
  }

  // Interrupt the in-flight turn. The wrapper interrupts the agent and emits the
  // aborted result over SSE, which clears `busy` (and `stopping`).
  async function stop() {
    if (!busy || stopping || archived) return;
    setStopping(true);
    setError(null);
    try {
      const res = await fetch(`/api/chat/${cloneId}/abort`, { method: "POST" });
      if (!res.ok) {
        const data = (await res.json().catch(() => ({}))) as { error?: string };
        throw new Error(data.error ?? "stop failed");
      }
      // Success: the SSE stream delivers the final (aborted) state from here.
    } catch (e) {
      setError((e as Error).message);
      setStopping(false);
    }
  }

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
                {formatWhen(Number(m.at))}
              </span>
              <span className="min-w-0 flex-1 truncate text-slate-600 dark:text-slate-300" title={m.text}>
                {m.text}
              </span>
              <button
                type="button"
                onClick={() => cancelScheduled(m.id)}
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
          if (picking) schedule();
          else send();
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
              min={epochMsToLocalInput(Date.now())}
              onChange={(e) => setScheduleAt(e.target.value)}
              className="min-w-0 flex-1 rounded-md border border-slate-300 bg-white px-2 py-1 text-sm text-slate-900 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100"
            />
            <button
              type="button"
              onClick={() => setScheduleAt("")}
              className="shrink-0 rounded-md border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-100 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-700"
            >
              Cancel
            </button>
          </div>
        ) : null}
        <div className="flex items-end gap-2">
          <textarea
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                if (picking) schedule();
                else send();
              }
            }}
            rows={2}
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
            onClick={() => setScheduleAt(picking ? "" : epochMsToLocalInput(Date.now() + 15 * 60 * 1000))}
            disabled={archived}
            title={picking ? "Send now instead" : "Schedule this message for later"}
            aria-pressed={picking}
            className={`shrink-0 rounded-md border px-2.5 py-2 text-sm disabled:opacity-40 ${
              picking
                ? "border-emerald-600 bg-emerald-50 text-emerald-700 dark:border-emerald-500 dark:bg-emerald-950/40 dark:text-emerald-400"
                : "border-slate-300 text-slate-500 hover:bg-slate-100 dark:border-slate-600 dark:text-slate-400 dark:hover:bg-slate-700"
            }`}
          >
            🕑
          </button>
          {picking ? (
            <button
              type="submit"
              disabled={archived || !input.trim() || !scheduleAt}
              className="shrink-0 rounded-md bg-emerald-600 px-3 py-2 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
            >
              Schedule
            </button>
          ) : busy && !archived ? (
            <button
              type="button"
              onClick={stop}
              disabled={stopping}
              title="Interrupt the agent's current turn"
              className="shrink-0 rounded-md bg-red-600 px-3 py-2 text-sm font-medium text-white hover:bg-red-700 disabled:opacity-50"
            >
              {stopping ? "Stopping…" : "Stop"}
            </button>
          ) : (
            <button
              type="submit"
              disabled={archived || !input.trim()}
              className="shrink-0 rounded-md bg-emerald-600 px-3 py-2 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
            >
              Send
            </button>
          )}
        </div>
      </form>
    </div>
  );
}
