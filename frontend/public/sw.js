// The service worker of the installed app. There is no offline copy:
//
// - HTML documents are not handled here, so each open loads the newest page from the network.
// - Code files (`/assets/*`, whose names change with each build) come from the network and are
//   kept as they load. After a deploy the server no longer has the old build's files, but iOS
//   can still show a page of that build and have it ask for one. The server says 404 and the
//   kept copy answers instead, so that page does not go white. Old copies are never deleted
//   all at once (that was the white screen in japan-trip), only the oldest past KEEP.
const CODE = "code";
const KEEP = 300;

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

self.addEventListener("fetch", (event) => {
  const req = event.request;
  const url = new URL(req.url);
  if (req.method !== "GET" || url.origin !== self.location.origin) return;
  if (!url.pathname.startsWith("/assets/")) return;
  event.respondWith(code(req));
});

// The page sends the code files it loaded before this worker took control of it (the first
// open), so those are kept too.
self.addEventListener("message", (event) => {
  if (event.data?.type !== "keep" || !Array.isArray(event.data.urls)) return;
  event.waitUntil(keep(event.data.urls));
});

async function code(req) {
  const cache = await caches.open(CODE);
  let res;
  try {
    res = await fetch(req);
  } catch (err) {
    const kept = await cache.match(req);
    if (kept) return kept;
    throw err;
  }
  if (res.ok) {
    await cache.put(req, res.clone());
    trim(cache);
    return res;
  }
  return (await cache.match(req)) ?? res;
}

async function keep(urls) {
  const cache = await caches.open(CODE);
  for (const href of urls) {
    const url = new URL(href, self.location.origin);
    if (url.origin !== self.location.origin || !url.pathname.startsWith("/assets/")) continue;
    if (await cache.match(url.href)) continue;
    try {
      const res = await fetch(url.href);
      if (res.ok) await cache.put(url.href, res);
    } catch {
      // the next open tries again
    }
  }
  await trim(cache);
}

async function trim(cache) {
  const keys = await cache.keys();
  for (const key of keys.slice(0, Math.max(0, keys.length - KEEP))) await cache.delete(key);
}
