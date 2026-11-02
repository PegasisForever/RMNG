import { tell } from "~/components/ConfirmLayer";

/** The name of monitor `id`'s window, which `openMonitorWindows` finds it by. */
export const monitorWindowName = (id: number) => `rmng-monitor-${id}`;

/** Open one browser window per monitor of `monitors`, each at `/?monitor=N`. Windows the
 *  browser blocks are told of in a message (`tell`), unless `sayBlocked` is false.
 *  Each is a plain window at the browser's default place and size; the operator moves, sizes and
 *  fullscreens it. A window that is open already stays as it is. */
export async function openMonitorWindows(monitors: { id: number }[], sayBlocked = true) {
  let blocked = 0;
  for (const m of monitors) {
    // `popup` makes it a window of its own rather than a tab. An empty URL finds a window of
    // this name that is open already without reloading it.
    const w = window.open("", monitorWindowName(m.id), "popup");
    if (!w) blocked++;
    else if (w.location.href === "about:blank") w.location.replace(`/?monitor=${m.id}`);
  }
  // Chrome and Safari block every pop-up after the first one of a click, and say so only with
  // a small icon in the address bar.
  // The message stays until the operator closes it; nothing here waits for that
  if (blocked > 0 && sayBlocked) {
    void tell(`The browser blocked ${blocked} monitor window${blocked === 1 ? "" : "s"}`, allowPopups());
  }
}

/** How to let this site open pop-ups, in the browser the operator uses. */
function allowPopups(): string {
  const ua = navigator.userAgent;
  if (/Firefox\//.test(ua)) {
    return `Click "Preferences" in the bar above the page, choose "Allow pop-ups for" this site, then try again.`;
  }
  // Safari: "Safari/" with no Chrome or Edge in the user agent.
  if (/Safari\//.test(ua) && !/Chrom(e|ium)\/|Edg\//.test(ua)) {
    return `In Safari, open Settings → Websites → Pop-up Windows, set this site to "Allow", then try again.`;
  }
  return `Click the blocked pop-up icon at the right of the address bar, choose "Always allow pop-ups", then try again.`;
}
