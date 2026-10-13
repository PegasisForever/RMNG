// The browser viewer's clipboard: each viewer window is one endpoint of the server's clipboard
// broker (docs/PROTOCOL.md "Clipboard (rich + lazy)"), as the native viewer's GTK clipboard is.
//
// Chrome lets a page use the clipboard only while the page has focus, and it has no "clipboard
// changed" event. So:
// - Local → clone: when the window gets focus, read the local clipboard. If it changed since a
//   viewer window last saw it, offer it to the server, keep the bytes, and answer the server's
//   requests from them.
// - Clone → local: when an offer arrives, ask at once for the best type of each kind (image,
//   HTML, text), as the native viewer does, and write them to the local clipboard. Only the
//   focused window does this; the others wait until they get focus. A write that Chrome
//   refuses is tried again on the next key press or click.
//
// All viewer windows share three values in localStorage: a hash of the last local content seen,
// the serial of the last offer a viewer window made, and when a window last acted on the
// clipboard. So focusing another window does not offer the same content again, a window does
// not fetch its sibling's offer, and an offer that one window wrote (or that a newer copy
// replaced) is not written again by the others. Offers are not told apart by serial and MIME
// list: two clones, or one clone after a restart, reuse the same small serials.
//
// Relative imports only, as in the rest of `viewer/`.
import type { ClipboardMsg } from "../lib/wire/ClipboardMsg";
import type { ClipboardOffer } from "../lib/wire/ClipboardOffer";
import type { ClipboardBytes } from "./protocol";

/** The types Chrome's clipboard can write, and so the only ones worth reading. */
export type LocalType = "image/png" | "text/html" | "text/plain";
const LOCAL_TYPES: LocalType[] = ["image/png", "text/html", "text/plain"];

/** Most bytes sent per type: what still fits the 32 MiB clipboard limit once it is base64 on
 *  the server's hop to the clone. A larger copy is answered with no bytes (the paste fails). */
export const MAX_BYTES = 24 * 1024 * 1024;

const SEEN_KEY = "rmng-viewer-clipboard-seen";
const OWN_KEY = "rmng-viewer-clipboard-own";
const ACTED_KEY = "rmng-viewer-clipboard-acted";

/** The clone's names for plain text (`crates/viewer`'s `is_text_mime`). */
export const isTextMime = (m: string) =>
  m.startsWith("text/plain") || m === "UTF8_STRING" || m === "TEXT";

/** The MIME types to offer the clone for local content of these Chrome types: plain text under
 *  the names Linux apps ask for. */
export function offerMimes(types: LocalType[]): string[] {
  return types.flatMap((t) =>
    t === "text/plain" ? ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"] : [t],
  );
}

/** The Chrome type that answers the clone's request for `mime`, or null for a type we never
 *  offer. */
export function localTypeFor(mime: string): LocalType | null {
  if (isTextMime(mime)) return "text/plain";
  return mime === "text/html" || mime === "image/png" ? mime : null;
}

/** What to fetch from a clone's offer (`crates/viewer`'s `pick_mimes`, limited to what Chrome
 *  can write): the PNG image, the HTML and the best plain text, each with the Chrome type it
 *  becomes. Plain text comes with the HTML, so a paste into a plain-text field still works. */
export function pickMimes(mimes: string[]): { mime: string; type: LocalType }[] {
  const out: { mime: string; type: LocalType }[] = [];
  const image = mimes.find((m) => m.startsWith("image/png"));
  if (image) out.push({ mime: image, type: "image/png" });
  if (mimes.includes("text/html")) out.push({ mime: "text/html", type: "text/html" });
  const text = mimes.find((m) => m.startsWith("text/plain;charset=utf-8")) ?? mimes.find(isTextMime);
  if (text) out.push({ mime: text, type: "text/plain" });
  return out;
}

function load(key: string) {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function save(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // No storage: windows may offer the same content twice, which is harmless.
  }
}

/** The local clipboard's content in the types Chrome can write; null when Chrome refuses the
 *  read (no focus, or no permission). */
async function readLocal(): Promise<Map<LocalType, Blob> | null> {
  try {
    const [item] = await navigator.clipboard.read();
    const blobs = new Map<LocalType, Blob>();
    for (const t of LOCAL_TYPES) {
      if (item?.types.includes(t)) blobs.set(t, await item.getType(t));
    }
    return blobs;
  } catch {
    return null;
  }
}

async function digest(blobs: Map<LocalType, Blob>) {
  const parts: BlobPart[] = [];
  for (const [t, b] of blobs) parts.push(t, "\0", b, "\0");
  const hash = await crypto.subtle.digest("SHA-256", await new Blob(parts).arrayBuffer());
  return Array.from(new Uint8Array(hash), (b) => b.toString(16).padStart(2, "0")).join("");
}

export interface ClipboardOptions {
  /** An offer or a request, as JSON. */
  send(msg: ClipboardMsg): void;
  /** Bytes for a request, as a binary message. */
  sendData(d: ClipboardBytes): void;
}

export interface ViewerClipboard {
  /** An offer or a request from the server. */
  onMessage(msg: ClipboardMsg): void;
  /** Bytes for a request this window made. */
  onData(d: ClipboardBytes): void;
  setConnected(connected: boolean): void;
  /** A key press or click: retry a write Chrome refused. */
  onGesture(): void;
  detach(): void;
}

export function attachClipboard(o: ClipboardOptions): ViewerClipboard {
  let connected = false;
  /** The local content this window offered (at `at`), kept to answer the server's requests. */
  let owned: { offer: ClipboardOffer; blobs: Map<LocalType, Blob>; at: number } | null = null;
  /** The newest offer from another endpoint that this window has not fetched, and when it came. */
  let remote: { offer: ClipboardOffer; at: number } | null = null;
  /** The fetch in progress: the types still to come (by the clone's MIME) and those that came. */
  let fetching: { serial: number; want: Map<string, LocalType>; got: Map<LocalType, Blob> } | null =
    null;
  /** Content Chrome refused to write, for the next gesture. */
  let unwritten: Map<LocalType, Blob> | null = null;

  // One clipboard operation at a time, in order: a read on focus must not run in the middle of
  // a write.
  let chain = Promise.resolve();
  const run = (f: () => Promise<void> | void) => {
    chain = chain.then(f).catch((e) => console.warn("clipboard:", e));
  };
  const focused = () => connected && document.hasFocus();
  /** Record that a window acted on the clipboard now; returns the time. */
  const acted = () => {
    const at = Date.now();
    save(ACTED_KEY, String(at));
    return at;
  };

  /** On focus: offer the local clipboard if it is new, else fetch an offer that came while the
   *  window had no focus. */
  async function syncLocal() {
    if (!focused()) return;
    const blobs = await readLocal();
    if (blobs?.size) {
      const hash = await digest(blobs);
      if (hash !== load(SEEN_KEY)) {
        save(SEEN_KEY, hash);
        const offer = { serial: Date.now(), mime_types: offerMimes([...blobs.keys()]) };
        save(OWN_KEY, String(offer.serial));
        owned = { offer, blobs, at: acted() };
        remote = null; // this copy is newer than any offer not fetched yet
        unwritten = null;
        o.send({ k: "offer", ...offer });
        return;
      }
    }
    fetchRemote();
  }

  function fetchRemote() {
    if (!remote || !focused()) return;
    const { offer, at } = remote;
    remote = null;
    // A window acted after this offer came: it fetched this offer, or a newer copy replaced it.
    if (Number(load(ACTED_KEY)) >= at) return;
    acted();
    const picks = pickMimes(offer.mime_types);
    if (!picks.length) return;
    unwritten = null;
    fetching = {
      serial: offer.serial,
      want: new Map(picks.map((p) => [p.mime, p.type])),
      got: new Map(),
    };
    for (const p of picks) o.send({ k: "request", serial: offer.serial, mime_type: p.mime });
  }

  async function write(blobs: Map<LocalType, Blob>) {
    try {
      await navigator.clipboard.write([new ClipboardItem(Object.fromEntries(blobs))]);
    } catch {
      unwritten = blobs; // no focus or no user activation: retry on the next gesture
      return;
    }
    // Remember what Chrome holds now (it re-encodes images and cleans HTML), so focusing a
    // window does not offer it back to the clone.
    const back = await readLocal();
    if (back?.size) save(SEEN_KEY, await digest(back));
  }

  async function serve(serial: number, mime: string) {
    const type = localTypeFor(mime);
    // The serial is not checked, as in the native viewer: the server sends a request only to
    // the current owner, and a clone can ask under the serial of the offer before (its paste
    // raced the new offer).
    const blob = type ? owned?.blobs.get(type) : undefined;
    let bytes = new Uint8Array(blob ? await blob.arrayBuffer() : new ArrayBuffer(0));
    if (bytes.length > MAX_BYTES) bytes = new Uint8Array(0);
    o.sendData({ serial, mimeType: mime, bytes });
  }

  const onFocus = () => run(syncLocal);
  const onVisibility = () => {
    if (document.visibilityState === "visible") run(syncLocal);
  };
  window.addEventListener("focus", onFocus);
  document.addEventListener("visibilitychange", onVisibility);

  return {
    onMessage(msg) {
      if (msg.k === "offer") {
        owned = null; // another endpoint owns the clipboard now
        if (String(msg.serial) === load(OWN_KEY)) return; // a sibling window's copy
        remote = { offer: { serial: msg.serial, mime_types: msg.mime_types }, at: Date.now() };
        run(fetchRemote);
      } else if (msg.k === "request") {
        void serve(msg.serial, msg.mime_type);
      }
    },
    onData(d) {
      const f = fetching;
      const type = f?.serial === d.serial ? f.want.get(d.mimeType) : undefined;
      if (!f || !type) return;
      f.want.delete(d.mimeType);
      if (d.bytes.length) f.got.set(type, new Blob([d.bytes], { type }));
      if (f.want.size) return;
      fetching = null;
      if (f.got.size) run(() => write(f.got));
    },
    setConnected(c) {
      connected = c;
      fetching = null;
      remote = null;
      if (!c) return;
      // The server forgot this window's offer when the old socket closed. If no window acted
      // since, it is still the newest copy: offer it again, so a paste in the clone finds it.
      // (The server's offer at connect is fetched again; that costs one transfer.)
      if (owned && Number(load(ACTED_KEY)) <= owned.at) {
        o.send({ k: "offer", ...owned.offer });
      }
      run(syncLocal);
    },
    onGesture() {
      if (!unwritten) return;
      const blobs = unwritten;
      unwritten = null;
      run(() => write(blobs));
    },
    detach() {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
    },
  };
}
