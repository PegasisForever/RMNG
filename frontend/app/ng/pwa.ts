// The installed /ng app's update rules. No offline copy; the newest build on each open:
// - The page always comes from the network (`public/ng-sw.js` does not handle pages, and the
//   server sends HTML with `Cache-Control: no-cache`).
// - The worker keeps each code file as it loads and answers with that copy when a later deploy
//   removed the file, so a page that iOS kept open from an older build does not go white.
// - An app that comes back to the front asks the server for its page. If that page belongs to
//   a different build, this one loads again; that reload gets only new files.

declare global {
  interface Window {
    /** Set when the /ng page shows; the safety net in `root.tsx` reads it. */
    __ngReady?: boolean;
    /** React Router's build manifest. Its URL changes with each build. */
    __reactRouterManifest?: { url?: string };
  }
}

/** The build manifest's file in a page of the app, e.g. `/assets/manifest-05e12003.js`. */
const MANIFEST = /\/assets\/manifest-[\w-]+\.js/;

async function newBuild(): Promise<boolean> {
  const mine = window.__reactRouterManifest?.url;
  if (!mine || !navigator.onLine) return false;
  try {
    const res = await fetch(location.pathname, { cache: "no-store" });
    if (!res.ok) return false;
    const theirs = (await res.text()).match(MANIFEST)?.[0];
    return theirs !== undefined && theirs !== mine;
  } catch {
    return false; // no answer: keep this build until the next time
  }
}

/** Registers the worker and checks for a new build each time the app comes to the front.
 *  Returns the cleanup. */
export function attachPwa(): () => void {
  window.__ngReady = true;
  let reg: ServiceWorkerRegistration | undefined;
  if ("serviceWorker" in navigator) {
    navigator.serviceWorker
      .register("/ng-sw.js", { scope: "/ng" })
      .then((r) => {
        reg = r;
        return navigator.serviceWorker.ready;
      })
      .then((ready) => {
        // The first open loaded its code before the worker ran: give the worker that list.
        const urls = performance
          .getEntriesByType("resource")
          .map((e) => e.name)
          .filter((u) => new URL(u).pathname.startsWith("/assets/"));
        ready.active?.postMessage({ type: "keep", urls });
      })
      .catch(() => {});
  }
  const onVisible = () => {
    if (document.visibilityState !== "visible") return;
    reg?.update().catch(() => {});
    void newBuild().then((yes) => yes && location.reload());
  };
  document.addEventListener("visibilitychange", onVisible);
  return () => document.removeEventListener("visibilitychange", onVisible);
}
