// Signs that this page may have been asleep or off the network. A connection that looked open
// before such a moment may be dead now (half-open: the browser sees no error, and nothing
// arrives), so the live connections (`./liveEvents.ts`, the media socket's worker) act at once
// instead of waiting for their slower watchdogs.

/** How often the sleep detector looks at the clock. */
export const WAKE_TICK_MS = 250;
/** A tick this much later than due means the machine slept (or the page was frozen). */
export const WAKE_GAP_MS = 3_000;

/** Call `fn` at each sign of a wake. `strong`: the machine slept (the clock jumped past a tick),
 *  the network came back (`online`), or the browser thawed the page or restored it from its
 *  back-forward cache; a connection from before it is likely dead. Not strong: the page came to
 *  the front or got the focus, which happens all the time. Returns the function that stops it.
 *  Wall-clock time, not `performance.now()`: on a Mac the monotonic clock stops while the
 *  machine sleeps. */
export function onWake(fn: (strong: boolean) => void): () => void {
  const strong = () => fn(true);
  const weak = () => fn(false);
  const onShow = (e: PageTransitionEvent) => fn(e.persisted);
  const onVisible = () => {
    if (document.visibilityState === "visible") fn(false);
  };
  window.addEventListener("online", strong);
  window.addEventListener("pageshow", onShow);
  window.addEventListener("focus", weak);
  document.addEventListener("visibilitychange", onVisible);
  // Page Lifecycle: Chrome thawed a frozen page
  document.addEventListener("resume", strong);
  let last = Date.now();
  const timer = window.setInterval(() => {
    const t = Date.now();
    const late = t - last - WAKE_TICK_MS;
    last = t;
    // A hidden page's timers are slowed down by the browser, which is no sleep
    if (late > WAKE_GAP_MS && document.visibilityState === "visible") fn(true);
  }, WAKE_TICK_MS);
  return () => {
    window.clearInterval(timer);
    window.removeEventListener("online", strong);
    window.removeEventListener("pageshow", onShow);
    window.removeEventListener("focus", weak);
    document.removeEventListener("visibilitychange", onVisible);
    document.removeEventListener("resume", strong);
  };
}
