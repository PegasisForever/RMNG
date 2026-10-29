// Lines from a browser into the control-server's log (`POST /api/client-log`), for what only the
// browser sees: a decoder that holds frames, a touch gesture the trackpad did not take. Sent in
// batches, at most every SEND_MS; a line that does not reach the server is lost, nothing more.
// Works in the page and in a worker. Relative imports only: the viewer worker uses it.

const SEND_MS = 2000;
/** Lines kept for the next batch at most; more are dropped until it goes. */
const MAX_LINES = 40;
const MAX_LINE = 600;

/** This page's id in the log, so one browser's lines read together. */
const id = Math.random().toString(36).slice(2, 8);
let lines: string[] = [];
let timer: ReturnType<typeof setTimeout> | undefined;
let first = true;

export function clientLog(source: string, line: string) {
  if (first) {
    first = false;
    lines.push(`${id} ${source}: start, ${navigator.userAgent}`);
  }
  if (lines.length >= MAX_LINES) return;
  lines.push(`${id} ${source}: ${line}`.slice(0, MAX_LINE));
  timer ??= setTimeout(() => {
    timer = undefined;
    const batch = lines;
    lines = [];
    fetch("/api/client-log", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ lines: batch }),
      keepalive: true,
    }).catch(() => {});
  }, SEND_MS);
}
