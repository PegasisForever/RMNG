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

export function Layout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <head>
        <meta charSet="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        {/* The installed app (Add to Home Screen) opens `/` full screen. The page draws under the
            iOS clock bar (`~/desk/page.ts`). */}
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
      <body>
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
  return (
    <main className="mx-auto max-w-2xl p-8">
      <h1 className="text-xl font-semibold text-red-600 dark:text-red-400">{message}</h1>
      {detail ? (
        <pre className="mt-4 overflow-auto rounded bg-slate-100 p-4 text-xs text-slate-500 dark:bg-slate-800 dark:text-slate-400">
          {detail}
        </pre>
      ) : null}
    </main>
  );
}
