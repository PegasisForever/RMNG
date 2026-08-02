// Where a Linear-hosted image points, on each side of one boundary.
//
// An image pasted into a ticket body is stored by Linear, because the body is: a
// `/uploads/<name>` URL resolves on this LAN and nowhere else, so an issue carrying one shows
// a broken image to everyone outside it. The cost lands here. A Linear `assetUrl` answers an
// unauthenticated GET with:
//
//     HTTP/2 401
//     {"error":"unauthorized","message":"Please provide authorization header compatible
//      with Linear GraphQL API"}
//
// and no redirect. An `<img>` cannot send that header. Neither can this page: preflighting
// `uploads.linear.app` answers `access-control-allow-headers: range`, and `authorization` is
// not in it, so fetching the bytes and building a blob URL is closed off too. Both measured.
//
// So the control-server has one read route, `GET /api/linear/asset?url=…`, which fetches the
// file with a preset's key and serves it same-origin. This module is the pair of functions
// that point markdown at it, and point it back.
//
// The boundary is the editor document. Markdown that a BlockNote editor holds carries the
// proxy path, because that is the only URL an `<img>` in this page can load. Markdown that
// crosses to Linear carries the original `https://uploads.linear.app/…`, because Linear's own
// clients and everyone else's browsers have to keep working. Every call site converts on the
// way in and converts back on the way out, so nothing stored anywhere knows about the proxy.

/** The control-server route that reads one Linear asset with a key and answers same-origin. */
export const ASSET_PROXY = "/api/linear/asset";

/** The one host the proxy will read from. The server pins the same name; this copy is what
 *  decides which URLs are worth sending there at all. */
export const LINEAR_ASSET_HOST = "uploads.linear.app";

/** True when `url` addresses a file the proxy can fetch.
 *
 *  `URL` is what decides the host, which is the same parser the browser would dial with, so
 *  `evil.tld@uploads.linear.app` reads as the allowed host here exactly as it would there,
 *  and case folding is done once by the parser rather than by a comparison here. */
export function isLinearAsset(url: string): boolean {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  return parsed.protocol === "https:" && parsed.hostname === LINEAR_ASSET_HOST;
}

/** `encodeURIComponent`, closed over the six characters it leaves alone.
 *
 *  It escapes everything but `A-Za-z0-9-_.!~*'()`, and `PROXY_URL` below stops a path at `'`
 *  or `)`. A URL carrying either one therefore mints a path that `toLinearMarkdown` cannot
 *  read back: `https://x'y@uploads.linear.app/a.png` proxies fine, reverses to nothing, and
 *  the proxy path itself reaches Linear as a broken image for the whole workspace.
 *
 *  Unreachable today, because `uploadToLinear` returns a `/uuid/uuid/uuid` asset URL. Closed
 *  anyway: what this costs is one `replace`, and what it buys is a pair that is total. */
function encodeAsset(url: string): string {
  return encodeURIComponent(url).replace(
    /[!'()*~]/g,
    (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`,
  );
}

/** The same-origin URL for one Linear asset. Anything else is handed back untouched, which
 *  is what leaves `/uploads/<name>` and every external image alone. */
export function assetProxyUrl(url: string): string {
  if (!isLinearAsset(url)) return url;
  return `${ASSET_PROXY}?url=${encodeAsset(url)}`;
}

/** The Linear URL a proxy path stands for, or the path unchanged when it is not one. */
export function linearAssetUrl(proxied: string): string {
  const prefix = `${ASSET_PROXY}?url=`;
  if (!proxied.startsWith(prefix)) return proxied;
  const encoded = proxied.slice(prefix.length);
  let decoded: string;
  try {
    decoded = decodeURIComponent(encoded);
  } catch {
    return proxied;
  }
  // A proxy path is only ever minted from a Linear URL. One that decodes to anything else was
  // typed by hand, and passing it through is safer than trusting it.
  return isLinearAsset(decoded) ? decoded : proxied;
}

// A URL ends where markdown or HTML says it does: whitespace, a closing paren or bracket, a
// quote, or an angle bracket. `&` is in the set for the proxy path alone, whose one parameter
// is always last, so a `&` after it belongs to somebody else.
const LINEAR_URL = `https://${LINEAR_ASSET_HOST.replace(/\./g, "\\.")}/[^\\s)\\]"'<>]+`;
const PROXY_URL = new RegExp(`${ASSET_PROXY.replace(/\//g, "\\/")}\\?url=[^\\s)\\]"'<>&]+`, "g");

// An IMAGE destination and nothing else. The proxy serves images, so an image is all it is
// ever pointed at, and the narrower the rewrite the less this route is reachable from.
//
// A link destination in particular is left where it is. `[click](https://uploads.linear.app/x)`
// rewritten is a one-click same-origin NAVIGATION to whatever that file turns out to be, on an
// origin that serves `/api/hosts/:id/exec` and `/api/delete` without a credential. Pointed at
// Linear it is a cross-origin navigation to somebody else's document, which is a link.
//
// `![alt](url)`, including the `![alt](<url>)` form and a trailing `"title"`, which the URL
// pattern stops before because it takes no whitespace. Alt text takes no `]` and no newline,
// so an unmatched `![` cannot reach across a line and swallow the link on the next one. Alt
// text that does hold a `]` leaves its own image un-proxied. That draws a broken image, which
// is the direction this should fail in.
const MD_IMAGE = new RegExp(`(!\\[[^\\]\\n]*\\]\\(\\s*<?)(${LINEAR_URL})`, "g");

// `<img src="…">`, quoted either way or bare, because a body from Linear can carry raw HTML.
// `[^>]*?` cannot cross the tag's own `>`, so the `src` matched is that `<img>`'s own.
const HTML_IMG = new RegExp(`(<img\\b[^>]*?\\bsrc\\s*=\\s*["']?)(${LINEAR_URL})`, "gi");

/** Markdown as an editor in this page should hold it: every Linear IMAGE behind the proxy.
 *
 *  Links, autolinks, and bare text keep pointing at Linear. See [`MD_IMAGE`]. */
export function toProxyMarkdown(markdown: string): string {
  const proxy = (_match: string, before: string, url: string) => `${before}${assetProxyUrl(url)}`;
  return markdown.replace(MD_IMAGE, proxy).replace(HTML_IMG, proxy);
}

/** Markdown as Linear should store it: every proxy path back to the URL it stands for.
 *
 *  Wider than its inverse on purpose. `toProxyMarkdown` mints a path only in an image
 *  destination, and this reverses one wherever it sits, so no shape an editor can produce
 *  leaves a `/api/linear/asset` path in a body Linear stores. */
export function toLinearMarkdown(markdown: string): string {
  return markdown.replace(PROXY_URL, (path) => linearAssetUrl(path));
}
