// An SSE stream that recovers by itself after a network drop or a sleep. The app's state
// stream and the chat stream both use it; each of their server routes sends a named `ping`
// event every PING_MS for the watchdog here to see.
import { onWake } from "~/lib/wake";

/** The server's heartbeat interval (`sse_heartbeat` in the control-server's `web.rs`). */
export const PING_MS = 5_000;

/** If no event or heartbeat has arrived for this long, treat the socket as wedged and rebuild
 *  it: two missed pings and some room for jitter. */
export const SSE_STALE_MS = 12_000;

/** After a weak sign of a wake (the page to the front, the focus), and for SSE_STALE_MS after
 *  it, a stream with nothing for longer than this missed a ping, so it is rebuilt. */
export const WAKE_STALE_MS = PING_MS + 2_500;

/** A strong sign of a wake within this long of the stream's start is the same wake (several
 *  signs come together): the stream is not rebuilt again. */
export const WAKE_AGAIN_MS = 1_000;

/** A connection not open in 3 times the last open's time (within these limits; the longest
 *  before the first open) is given up and opened again: one started while the network was gone
 *  (a laptop waking up, a tunnel such as Tailscale whose packets go nowhere) can wait for good.
 *  Every second try in a row that times out gets twice as long, for a link slower than it was. */
export const CONNECT_MIN_MS = 1_500;
export const CONNECT_MAX_MS = 8_000;

/** After an error, open again after this long, doubled at each error in a row up to
 *  RETRY_MAX_MS. The browser's own retry waits seconds, so the stream does its own; short, as
 *  while the network comes back each try fails fast, and the first after it should come soon. */
export const RETRY_FIRST_MS = 250;
export const RETRY_MAX_MS = 500;

/** How often the watchdog looks. */
export const WATCHDOG_MS = 250;

/** The handlers of one stream, by event name: `message` for unnamed events. Each gets the
 *  event's `data`. A `ping` handler is not needed: every event, `ping` too, counts as life. */
export type LiveHandlers = Record<string, (data: string) => void>;

/** Open `url` as an `EventSource` that stays live; returns the function that closes it for good.
 *
 *  A plain `EventSource` auto-reconnects when the browser *notices* the socket died, and only
 *  after a wait of its own. But after a Wi-Fi drop or a sleep the TCP connection often goes
 *  half-open: it stays `OPEN`, delivers nothing, and never fires `onerror`, so the UI silently
 *  stops updating. So: a strong sign of a wake (the laptop awake, the network back, the page
 *  thawed: `~/lib/wake.ts`) rebuilds the stream at once; an error opens again within
 *  RETRY_MAX_MS; a connection that does not open is given up; and a staleness watchdog rebuilds
 *  a stream with no ping (sooner after a weak sign of a wake). A fresh connection replays the
 *  full snapshot, so the UI resyncs on reconnect, and the old state stays on screen until it
 *  arrives. */
export function openLiveEvents(url: string, handlers: LiveHandlers): () => void {
  let es: EventSource | null = null;
  /** When the last event (or the open) arrived, and when this connection was started. */
  let lastActivity = Date.now();
  let startedAt = Date.now();
  let opened = false;
  /** How long the last open took (null: none yet), and the opens in a row that timed out. */
  let connectTook: number | null = null;
  let timeouts = 0;
  let retryMs = 0;
  let retry: number | undefined;
  /** The last weak sign of a wake. */
  let wokeAt = -Infinity;
  let disposed = false; // set on close so late callbacks don't reopen

  const connectLimit = () => {
    const base =
      connectTook === null
        ? CONNECT_MAX_MS
        : Math.min(Math.max(connectTook * 3, CONNECT_MIN_MS), CONNECT_MAX_MS);
    return timeouts % 2 ? Math.min(base * 2, CONNECT_MAX_MS) : base;
  };

  const connect = () => {
    if (disposed) return;
    window.clearTimeout(retry);
    retry = undefined;
    es?.close();
    const s = new EventSource(url);
    es = s;
    startedAt = lastActivity = Date.now();
    opened = false;
    const alive = () => {
      if (es !== s) return false;
      lastActivity = Date.now();
      if (!opened) {
        opened = true;
        connectTook = lastActivity - startedAt;
        timeouts = 0;
      }
      return true;
    };
    s.onopen = () => {
      alive();
    };
    s.onerror = () => {
      if (es !== s) return;
      // Ours to retry, sooner than the browser would
      s.close();
      es = null;
      retry = window.setTimeout(connect, retryMs);
      retryMs = Math.min(retryMs * 2 || RETRY_FIRST_MS, RETRY_MAX_MS);
    };
    for (const name of new Set([...Object.keys(handlers), "ping"])) {
      s.addEventListener(name, (e) => {
        if (!alive()) return;
        // The server spoke, so the next error retries at once
        retryMs = 0;
        handlers[name]?.((e as MessageEvent).data);
      });
    }
  };

  connect();

  // Watchdog: rebuild a stream that has gone silent past the staleness window (the half-open
  // case), or one that does not open.
  let tickAt = Date.now();
  const watchdog = window.setInterval(() => {
    const now = Date.now();
    // The clock was set back: the times kept are in the future, so start them again
    if (now < tickAt) startedAt = lastActivity = wokeAt = now;
    tickAt = now;
    if (disposed || !es) return;
    if (!opened) {
      if (now - startedAt <= connectLimit()) return;
      timeouts++;
      connect();
    } else {
      const limit = now - wokeAt < SSE_STALE_MS ? WAKE_STALE_MS : SSE_STALE_MS;
      if (es.readyState === EventSource.CLOSED || now - lastActivity > limit) connect();
    }
  }, WATCHDOG_MS);

  // A strong sign: the stream from before it is likely dead, and finding out costs seconds, so
  // rebuild it now (a snapshot is cheap). A weak one: rebuild a stream that missed a ping; and
  // for a while after, the next ping that does not come
  // rebuilds it (the watchdog's shorter limit). Either way one waiting to retry tries now.
  const stopWake = onWake((strong) => {
    if (disposed) return;
    const now = Date.now();
    if (!es) {
      retryMs = 0;
      return connect();
    }
    if (strong) {
      if (now - startedAt < WAKE_AGAIN_MS) return;
      timeouts = 0;
      retryMs = 0;
      return connect();
    }
    wokeAt = now;
    if (opened && now - lastActivity > WAKE_STALE_MS) {
      retryMs = 0;
      connect();
    }
  });

  return () => {
    disposed = true;
    window.clearInterval(watchdog);
    window.clearTimeout(retry);
    stopWake();
    es?.close();
  };
}
