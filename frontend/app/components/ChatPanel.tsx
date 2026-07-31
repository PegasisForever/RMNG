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
//
// This module is the network half only. The markup lives in ChatView, which takes the
// thread and the composer state as props.
import { useEffect, useState } from "react";

import { ChatView, localInputToEpochMs } from "~/components/ChatView";
import type { ChatMessage } from "~/lib/types";
import type { ScheduledMessage } from "~/lib/wire/ScheduledMessage";

export { epochMsToLocalInput, localInputToEpochMs } from "~/components/ChatView";

interface ChatSnapshot {
  busy: boolean;
  activity?: string;
  messages: ChatMessage[];
  scheduled?: ScheduledMessage[];
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
    <ChatView
      messages={messages}
      loading={loading}
      busy={busy}
      stopping={stopping}
      activity={activity}
      error={error}
      archived={archived}
      scheduled={scheduled}
      input={input}
      onInputChange={setInput}
      scheduleAt={scheduleAt}
      onScheduleAtChange={setScheduleAt}
      onSend={send}
      onSchedule={schedule}
      onStop={stop}
      onCancelScheduled={cancelScheduled}
    />
  );
}
