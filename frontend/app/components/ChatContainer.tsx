// Per-clone chat with the in-container agent (Claude Agent SDK). Client-only, lazy-imported
// and keyed by clone id (same pattern as NotesEditorContainer). Subscribes to the per-clone
// chat SSE (/api/chat/:id/events) for { busy, messages, scheduled }, so the agent's reply
// and the "working" indicator survive a refresh — the POST only kicks the turn
// off; the reply lands over SSE. Posting a message is fire-and-forget.
//
// Messages can also be *scheduled*: the operator picks a wall-clock time and the server
// delivers it later from a disk-backed queue (surviving a closed browser and a control-server
// restart). The pending queue rides the same SSE frame, so a cancel in one tab is reflected
// in every other one with no extra stream to manage here.
//
// This is the container half: the stream, the four calls behind it, the draft store, the
// clock, and the operator's locale. Every one of those is a thing a story cannot have, which
// is why they all live here and nothing below ChatView knows about any of them. The markup is
// ChatView, which takes the thread and the composer state as props and is the half Storybook
// renders.
import { useCallback, useEffect, useState } from "react";

import { ChatView, localInputToEpochMs } from "~/components/ChatView";
import { getDraft, setDraft } from "~/lib/chatDrafts";
import { chatErrorText } from "~/lib/chatError";
import { browserLocale } from "~/lib/format";
import type { ChatMessage } from "~/lib/types";
import type { ScheduledMessage } from "~/lib/wire/ScheduledMessage";

interface ChatSnapshot {
  busy: boolean;
  activity?: string;
  messages: ChatMessage[];
  scheduled?: ScheduledMessage[];
}

/** What the banner says when one of the four calls below fails.
 *
 *  Every chat route answers an error as axum `(StatusCode, String)`, so the body is plain text
 *  and reading it as JSON would throw away the one sentence the operator needs. `chatErrorText`
 *  decides how much of it the banner can hold. A body that never arrives (the connection
 *  dropped mid-read) is the same case as an empty one: `fallback`. */
async function errorText(res: Response, fallback: string): Promise<string> {
  return chatErrorText(await res.text().catch(() => ""), fallback);
}

export default function ChatContainer({
  cloneId,
  archived = false,
}: {
  cloneId: string;
  archived?: boolean;
}) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [loading, setLoading] = useState(true);
  // Composer text, mirrored into the per-clone draft store. This panel is keyed by clone id,
  // so it remounts on every clone switch; the store is what carries the unsent text across
  // that remount. Every write goes through `writeInput`, so clearing the box on send (or
  // restoring it on a failed send) keeps the store in step.
  const [input, setInput] = useState(() => getDraft(cloneId));
  const writeInput = useCallback(
    (text: string) => {
      setInput(text);
      setDraft(cloneId, text);
    },
    [cloneId],
  );
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
    writeInput("");
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
      // "unknown clone 'x'" (400), "clone 'x' is archived; unarchive it first" (409), or the
      // reason the turn could not start ("a message is already being processed for this clone").
      if (!res.ok) throw new Error(await errorText(res, "chat failed"));
      // Success: nothing to do — the SSE stream delivers the authoritative
      // messages and busy state from here.
    } catch (e) {
      setError((e as Error).message);
      writeInput(text); // restore the unsent text
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
      if (!res.ok) throw new Error(await errorText(res, "schedule failed"));
      writeInput("");
      setScheduleAt(""); // collapse the picker back down
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function cancelScheduled(sid: string) {
    setError(null);
    try {
      const res = await fetch(`/api/chat/${cloneId}/schedule/${sid}`, { method: "DELETE" });
      // 404 means it already fired or another tab cancelled it — SSE has the truth either way.
      // Anything else is the server's ("invalid clone id 'x'"), so it goes to the banner as is.
      if (!res.ok && res.status !== 404) throw new Error(await errorText(res, "cancel failed"));
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
      // The abort route's one refusal is an archived clone. An unknown id answers 204.
      if (!res.ok) throw new Error(await errorText(res, "stop failed"));
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
      onInputChange={writeInput}
      scheduleAt={scheduleAt}
      onScheduleAtChange={setScheduleAt}
      onSend={send}
      onSchedule={schedule}
      onStop={stop}
      onCancelScheduled={cancelScheduled}
      // The clock and the locale are read here, on the container's side of the seam, so the
      // same props always draw the same pane. A story hands over a fixed instant and a fixed
      // locale instead.
      now={Date.now()}
      locale={browserLocale()}
    />
  );
}
