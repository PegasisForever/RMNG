// An SSE stream that recovers by itself after a network drop or a sleep. The app's state
// stream and the chat stream both use it; each of their server routes sends a named `ping`
// event every 15s for the watchdog here to see.

/** If no event or heartbeat has arrived for this long, treat the socket as wedged and rebuild
 *  it. The server pings every 15s, so ~3 missed pings — comfortably clear of jitter but quick
 *  enough to recover. */
export const SSE_STALE_MS = 45_000;

/** How often the watchdog looks. */
export const WATCHDOG_MS = 5_000;

/** The handlers of one stream, by event name: `message` for unnamed events. Each gets the
 *  event's `data`. A `ping` handler is not needed: every event, `ping` too, counts as life. */
export type LiveHandlers = Record<string, (data: string) => void>;

/** Open `url` as an `EventSource` that stays live; returns the function that closes it for good.
 *
 *  A plain `EventSource` auto-reconnects when the browser *notices* the socket died — but
 *  after a Wi-Fi drop the TCP connection often goes half-open: it stays `OPEN`, delivers
 *  nothing, and never fires `onerror`, so the UI silently stops updating. We defend against
 *  that with an observable heartbeat + three recovery triggers: a staleness watchdog, the
 *  `online` event (Wi-Fi came back), and tab re-focus. Each rebuilds the connection, and a
 *  fresh connection replays the full snapshot, so the UI resyncs on reconnect. */
export function openLiveEvents(url: string, handlers: LiveHandlers): () => void {
  let es: EventSource | null = null;
  let lastActivity = Date.now();
  let disposed = false; // set on close so late callbacks don't reopen

  const connect = () => {
    if (disposed) return;
    es?.close();
    lastActivity = Date.now();
    es = new EventSource(url);
    es.onopen = () => {
      lastActivity = Date.now();
    };
    for (const name of new Set([...Object.keys(handlers), "ping"])) {
      es.addEventListener(name, (e) => {
        lastActivity = Date.now();
        handlers[name]?.((e as MessageEvent).data);
      });
    }
  };

  connect();

  // Watchdog: rebuild a socket the browser has given up on (CLOSED), or one that has gone
  // silent past the staleness window, OPEN (the half-open case) or CONNECTING alike. A
  // CONNECTING socket is usually the browser's own retry, which ends in seconds; but one
  // opened while the network was still gone (a laptop waking up) can wait on a dead
  // connection for good and never fire an event, and the page then stays old until a refresh.
  const watchdog = window.setInterval(() => {
    if (disposed) return;
    const stale = Date.now() - lastActivity > SSE_STALE_MS;
    if (es?.readyState === EventSource.CLOSED || stale) {
      connect();
    }
  }, WATCHDOG_MS);

  // Wi-Fi/network regained → rebuild immediately (the current socket is likely half-open).
  const onOnline = () => connect();
  // Tab re-focus after a sleep/background stretch that outran the staleness window.
  const onVisible = () => {
    if (
      document.visibilityState === "visible" &&
      Date.now() - lastActivity > SSE_STALE_MS
    ) {
      connect();
    }
  };
  window.addEventListener("online", onOnline);
  document.addEventListener("visibilitychange", onVisible);

  return () => {
    disposed = true;
    window.clearInterval(watchdog);
    window.removeEventListener("online", onOnline);
    document.removeEventListener("visibilitychange", onVisible);
    es?.close();
  };
}
