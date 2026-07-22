// Per-host chat with the in-container agent (Claude Agent SDK). Client-only, lazy-imported
// and keyed by host id (same pattern as HostEditor). Subscribes to the per-host
// chat SSE (/api/chat/:id/events) for { busy, messages }, so the agent's reply
// and the "working" indicator survive a refresh — the POST only kicks the turn
// off; the reply lands over SSE. Posting a message is fire-and-forget.
import { useEffect, useRef, useState } from "react";

import type { ChatMessage } from "~/lib/types";

interface ChatSnapshot {
  busy: boolean;
  activity?: string;
  messages: ChatMessage[];
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

export default function ChatPanel({ hostId, archived = false }: { hostId: string; archived?: boolean }) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [activity, setActivity] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  // Live thread + turn state over SSE. EventSource auto-reconnects, so a refresh
  // (or a transient drop) re-syncs from the server's snapshot — including an
  // in-flight turn started by a previous page load.
  useEffect(() => {
    setLoading(true);
    setError(null);
    const es = new EventSource(`/api/chat/${hostId}/events`);
    es.onmessage = (e) => {
      try {
        const snap = JSON.parse(e.data) as ChatSnapshot;
        setMessages(Array.isArray(snap.messages) ? snap.messages : []);
        setBusy(!!snap.busy);
        if (!snap.busy) setStopping(false); // turn ended — reset the Stop button
        setActivity(snap.activity ?? null);
        setLoading(false);
      } catch {
        // ignore malformed frame
      }
    };
    return () => es.close();
  }, [hostId]);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, busy]);

  // `override` lets buttons send a fixed message (e.g. "monitor") instead of the
  // textarea contents. Falls back to the trimmed input otherwise. Fire-and-forget:
  // the POST only starts the turn; the reply and final busy state arrive via SSE.
  async function send(override?: string) {
    const text = (override ?? input).trim();
    if (!text || busy || archived) return;
    if (override === undefined) setInput("");
    setError(null);
    setBusy(true); // optimistic; the SSE snapshot confirms (or clears) it
    setActivity(null);
    // Optimistic user bubble; the server snapshot replaces it once it arrives.
    setMessages((m) => [...m, { id: `tmp-${Date.now()}`, role: "user", text, ts: Date.now() }]);
    try {
      const res = await fetch(`/api/chat/${hostId}`, {
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
      if (override === undefined) setInput(text); // restore the unsent text
      setBusy(false); // the turn never started; SSE will reconcile messages
    }
  }

  // Interrupt the in-flight turn. The wrapper interrupts the agent and emits the
  // aborted result over SSE, which clears `busy` (and `stopping`).
  async function stop() {
    if (!busy || stopping || archived) return;
    setStopping(true);
    setError(null);
    try {
      const res = await fetch(`/api/chat/${hostId}/abort`, { method: "POST" });
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
              : "Ask the agent anything — it can control this host's desktop."}
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

      <form
        className="flex items-end gap-2 border-t border-slate-200 p-2 dark:border-slate-700"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
          rows={2}
          placeholder={archived ? "Unarchive to message the agent" : "Message the agent…  (Enter to send)"}
          disabled={busy || archived}
          className="min-w-0 flex-1 resize-none rounded-md border border-slate-300 bg-white px-2 py-1.5 text-sm text-slate-900 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none disabled:opacity-60 dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500"
        />
        <button
          type="button"
          onClick={() => send("monitor")}
          disabled={busy || archived}
          title={archived ? "Unarchive this clone before monitoring it" : "Tell the agent to start monitoring this desktop (track working vs idle)"}
          className="shrink-0 rounded-md border border-amber-400 bg-amber-50 px-3 py-2 text-sm font-medium text-amber-700 hover:bg-amber-100 disabled:opacity-40 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-400 dark:hover:bg-amber-900/40"
        >
          Monitor
        </button>
        {busy && !archived ? (
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
      </form>
    </div>
  );
}
