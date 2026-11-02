import {
  Links,
  Meta,
  Outlet,
  Scripts,
  ScrollRestoration,
} from "react-router";

import type { Route } from "./+types/root";
import "./app.css";

export function meta() {
  return [{ title: "RMNG" }];
}

// Safety net for the installed app: no page 8 s after opening means its kept code files do not
// match. Remove the worker and its copies and load once more from the network (once per open,
// so it cannot loop). `~/desk/pwa.ts` sets `__appReady` when the app shows.
const SAFETY_NET = `
setTimeout(function () {
  if (window.__appReady) return;
  try {
    if (sessionStorage.getItem("healed")) return;
    sessionStorage.setItem("healed", "1");
  } catch (e) {}
  var jobs = [];
  if ("serviceWorker" in navigator)
    jobs.push(navigator.serviceWorker.getRegistrations().then(function (rs) {
      return Promise.all(rs.map(function (r) { return r.unregister(); }));
    }));
  if (window.caches) jobs.push(caches.delete("code"));
  Promise.all(jobs).then(function () { location.reload(); }, function () { location.reload(); });
}, 8000);
`;

/** Black under the page too: iOS shows the strip below the page size. On iOS 26+ the installed
 *  app gets a page shorter than the screen while the document is shorter than the screen, and
 *  nothing is painted below that edge (WebKit bug 301108; the fix from frappe-ui#1284): a
 *  document as tall as the screen gets the whole screen. Here in the page itself, so both hold
 *  from the first paint. */
const HTML_STYLE = { background: "#000", minHeight: "min(100lvh, calc(100% + env(safe-area-inset-top)))" };

export function Layout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" style={HTML_STYLE}>
      <head>
        <meta charSet="utf-8" />
        {/* The page takes the whole screen, under the clock bar and the home bar, and cannot be
            zoomed. The installed app (Add to Home Screen) opens `/` full screen. */}
        <meta
          name="viewport"
          content="width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no, viewport-fit=cover"
        />
        <link rel="manifest" href="/manifest.webmanifest" />
        <link rel="apple-touch-icon" href="/icons/icon-180.png" />
        <meta name="apple-mobile-web-app-capable" content="yes" />
        <meta name="mobile-web-app-capable" content="yes" />
        <meta name="apple-mobile-web-app-status-bar-style" content="black-translucent" />
        <meta name="apple-mobile-web-app-title" content="RMNG" />
        <script dangerouslySetInnerHTML={{ __html: SAFETY_NET }} />
        <Meta />
        <Links />
      </head>
      <body className="overflow-hidden">
        {children}
        <ScrollRestoration />
        <Scripts />
      </body>
    </html>
  );
}

export default function App() {
  return <Outlet />;
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  let message = "Something went wrong";
  let detail = "";
  if (error instanceof Error) {
    message = error.message;
    detail = error.stack ?? "";
  }
  // On the page's black ground, in a box of its own that scrolls (the page does not). The ground
  // is black in both looks, so the colors are the dark ones (`~/desk/GLASS.md`).
  return (
    <main className="fixed inset-0 select-text overflow-auto p-8">
      <div className="mx-auto max-w-2xl">
        <h1 className="text-xl font-semibold text-rose-300">{message}</h1>
        {detail ? <pre className="mt-4 overflow-auto rounded-[10px] border border-white/10 bg-white/[0.06] p-4 text-xs text-white/60">{detail}</pre> : null}
      </div>
    </main>
  );
}
