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

/** The host whose files need the proxy at all. Everything else an editor holds is either
 *  already same-origin or already loadable, so this is what decides where a rewrite applies. */
export const LINEAR_ASSET_HOST = "uploads.linear.app";

/** True when `url` addresses a file only the proxy can fetch.
 *
 *  `URL` is what decides the host, which is the same parser the browser would dial with, so
 *  case folding and userinfo are handled once by the parser rather than by a comparison
 *  here. */
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
 *  or `)`. A URL carrying either one would otherwise mint a path that `toLinearMarkdown`
 *  cannot read back, and that path reaches Linear as a broken image for the whole workspace.
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

/** The URL a proxy path stands for, or the path unchanged when it is not one.
 *
 *  Every path this reads reverses, whatever it decodes to. A proxy path that survived into a
 *  Linear body would be a broken image for everyone in the workspace, so the reverse stays
 *  total rather than picking which paths it is willing to undo. */
export function linearAssetUrl(proxied: string): string {
  const prefix = `${ASSET_PROXY}?url=`;
  if (!proxied.startsWith(prefix)) return proxied;
  try {
    return decodeURIComponent(proxied.slice(prefix.length));
  } catch {
    return proxied;
  }
}

// A URL ends where markdown or HTML says it does: whitespace, a closing paren or bracket, a
// quote, or an angle bracket. `&` is in the set for the proxy path alone, whose one parameter
// is always last, so a `&` after it belongs to somebody else.
const LINEAR_URL = `https://${LINEAR_ASSET_HOST.replace(/\./g, "\\.")}/[^\\s)\\]"'<>]+`;
const PROXY_URL = new RegExp(`${ASSET_PROXY.replace(/\//g, "\\/")}\\?url=[^\\s)\\]"'<>&]+`, "g");

// An IMAGE destination and nothing else, which is the whole of what the proxy is for: an
// `<img>` source has to be same-origin, and nothing else in a body does.
//
// A link destination keeps pointing at Linear, and so does a bare URL in prose. Both stay
// readable that way, and both already work in a browser without a key.
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
