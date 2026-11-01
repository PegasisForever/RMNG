// How the pages take the browser's page: the whole screen, under the clock bar and the home
// bar, not zoomable, black under it all. `routes/app.tsx` (the app) and `routes/screenArea.tsx`
// (the screen area test) both do it, so the test shows what the app gets.

/** The page runs under the clock bar and the home bar, and cannot be zoomed. */
const VIEWPORT = "width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no, viewport-fit=cover";

/** Take the page; returns what puts it back. */
export function takePage(): () => void {
  const meta = document.querySelector<HTMLMetaElement>('meta[name="viewport"]');
  const before = meta?.content;
  if (meta) meta.content = VIEWPORT;
  // Black under the page too: iOS shows the strip below the page size
  const { documentElement: html, body } = document;
  html.style.background = body.style.background = "#000";
  body.style.overflow = "hidden";
  // iOS 26+ gives an installed app a page shorter than the screen while the document is
  // shorter than the screen, and paints nothing below that edge (WebKit bug 301108; the fix
  // from frappe-ui#1284). A document as tall as the screen gets the whole screen.
  html.style.minHeight = "min(100lvh, calc(100% + env(safe-area-inset-top)))";
  return () => {
    if (meta && before !== undefined) meta.content = before;
    html.style.background = body.style.background = "";
    html.style.minHeight = "";
    body.style.overflow = "";
  };
}
