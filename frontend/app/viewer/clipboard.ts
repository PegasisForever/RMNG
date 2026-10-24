// The browser viewer's clipboard: each viewer window is one endpoint of the server's clipboard
// broker (docs/PROTOCOL.md "Clipboard (rich + lazy)").
//
// Chrome lets a page use the clipboard only while the page has focus, and it has no "clipboard
// changed" event. So:
// - Local → clone: when the window gets focus, read the local clipboard. If it changed since a
//   viewer window last saw it, offer it to the server, keep the bytes, and answer the server's
//   requests from them.
// - Clone → local: an offer becomes the shared "pending" offer. The focused window asks at once
//   for the best type of each kind (image, HTML, text), and writes
//   what came after each reply. With no window focused, the next window that gets focus does
//   it, so an agent's copy still reaches the local clipboard. A write that Chrome refuses is
//   tried again on the next key press or click.
//
// Safari and Firefox read the clipboard for a page only in the paste event of a paste chord
// (Cmd+V on a Mac, Ctrl+V elsewhere), and write it only while the page handles a key press or
// a click (Selkies does the same). There:
// - Local → clone: the paste chord's paste event offers what it carries. The chord's V waits
//   for that offer (`InputOptions.pasteByEvent`), so the clone pastes the new content.
// - Clone → local: the copy chord (Cmd+C or Cmd+X) writes at once a plain text item whose
//   content comes later: the clone's text, once the offer of its copy and the text arrive.
//   Everything else (HTML, an image, an agent's copy) waits for the next key press or click.
//
// All viewer windows share two values in localStorage: a hash of the last local content seen
// (so focusing another window does not offer the same content again), and the pending offer.
// A window's own offer is stored there marked `own`, so its siblings, which get it from the
// server too, do not fetch it back.
//
// Relative imports only, as in the rest of `viewer/`.
import type { ClipboardMsg } from "../lib/wire/ClipboardMsg";
import type { ClipboardOffer } from "../lib/wire/ClipboardOffer";
import type { ClipboardBytes } from "./protocol";

/** The types Chrome's clipboard can write, and so the only ones worth reading. */
type LocalType = "image/png" | "text/html" | "text/plain";
const LOCAL_TYPES: LocalType[] = ["image/png", "text/html", "text/plain"];

/** How long a copy chord's write waits for the clone's text before it gives up. */
const COPY_WAIT_MS = 2000;

/** Most bytes sent per type: what still fits the 32 MiB clipboard limit once it is base64 on
 *  the server's hop to the clone. A larger copy is answered with no bytes (the paste fails). */
const MAX_BYTES = 23 * 1024 * 1024;

const SEEN_KEY = "rmng-viewer-clipboard-seen";
const PENDING_KEY = "rmng-viewer-clipboard-pending";

/** The shared pending offer: one from another endpoint that no window has written yet, or
 *  (`own`) the one a viewer window made. */
type Pending = ClipboardOffer & { own?: boolean };

/** The clone's names for plain text. */
const isTextMime = (m: string) =>
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

/** What to fetch from a clone's offer (limited to what Chrome can write): the PNG image, the HTML and the best plain text, each with the Chrome type it
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

const pending = (): Pending | null => JSON.parse(localStorage.getItem(PENDING_KEY) ?? "null");
const setPending = (p: Pending | null) => localStorage.setItem(PENDING_KEY, JSON.stringify(p));

/** The local clipboard's content in the types Chrome can write; null when Chrome refuses the
 *  read (no focus, or no permission). Some apps (Slack) copy HTML with no plain text, which
 *  a Mac text box turns into text on paste but a clone's terminal cannot paste; so plain text
 *  is made from the HTML when there is none. */
async function readLocal(): Promise<Map<LocalType, Blob> | null> {
  try {
    const [item] = await navigator.clipboard.read();
    const blobs = new Map<LocalType, Blob>();
    for (const t of LOCAL_TYPES) {
      if (item?.types.includes(t)) blobs.set(t, await item.getType(t));
    }
    const html = blobs.get("text/html");
    if (html && !blobs.get("text/plain")?.size) {
      blobs.set("text/plain", new Blob([htmlToText(await html.text())], { type: "text/plain" }));
    }
    return blobs;
  } catch {
    return null;
  }
}

/** The text a paste of `html` into a plain text field gives. `innerText` follows the layout
 *  (blocks and `<br>` become line breaks), so the HTML is put off-screen in the page for a
 *  moment; images become their alt text and styles, scripts and frames are dropped first, so
 *  nothing loads and the page's own look does not change. */
export function htmlToText(html: string): string {
  const doc = new DOMParser().parseFromString(html, "text/html");
  for (const img of doc.querySelectorAll("img")) img.replaceWith(img.alt);
  for (const e of doc.querySelectorAll("script,style,link,meta,iframe,object,embed,video,audio")) {
    e.remove();
  }
  const box = document.createElement("div");
  box.style.cssText = "position:fixed;left:-10000px;top:0;white-space:normal";
  box.append(...doc.body.childNodes);
  document.body.append(box);
  const text = box.innerText;
  box.remove();
  return text;
}

/** The content of a paste event, in the types Chrome can write (as `readLocal`). */
function pasted(cd: DataTransfer): Map<LocalType, Blob> {
  const blobs = new Map<LocalType, Blob>();
  const image = [...cd.items].find((i) => i.kind === "file" && i.type === "image/png");
  const file = image?.getAsFile();
  if (file) blobs.set("image/png", file);
  const html = cd.getData("text/html");
  if (html) blobs.set("text/html", new Blob([html], { type: "text/html" }));
  const text = cd.getData("text/plain") || (html ? htmlToText(html) : "");
  if (text) blobs.set("text/plain", new Blob([text], { type: "text/plain" }));
  return blobs;
}

async function digest(blobs: Map<LocalType, Blob>) {
  const parts: BlobPart[] = [];
  for (const [t, b] of blobs) parts.push(t, "\0", b, "\0");
  const hash = await crypto.subtle.digest("SHA-256", await new Blob(parts).arrayBuffer());
  return Array.from(new Uint8Array(hash), (b) => b.toString(16).padStart(2, "0")).join("");
}

interface ClipboardOptions {
  /** An offer or a request, as JSON. */
  send(msg: ClipboardMsg): void;
  /** Bytes for a request (the worker sends them in pieces). Takes `d.bytes`' buffer. */
  sendData(d: ClipboardBytes): void;
}

interface ViewerClipboard {
  /** An offer or a request from the server. */
  onMessage(msg: ClipboardMsg): void;
  /** Bytes for a request this window made. */
  onData(d: ClipboardBytes): void;
  setConnected(connected: boolean): void;
  /** A key press or click: retry a write the browser refused. */
  onGesture(): void;
  /** Whether the local clipboard comes only from a paste chord's paste event. */
  pasteByEvent(): boolean;
  detach(): void;
}

export function attachClipboard(o: ClipboardOptions): ViewerClipboard {
  let connected = false;
  /** The local content this window offered last, kept to answer the server's requests. The
   *  server sends requests only to the current owner, so it is never cleared. */
  let owned: Map<LocalType, Blob> | null = null;
  /** The fetch in progress: the types asked for (by the clone's MIME) and those that came, and
   *  whether a copy chord's write took its text (`onCopyKey`). */
  let fetching: {
    serial: number;
    want: Map<string, LocalType>;
    got: Map<LocalType, Blob>;
    byChord?: boolean;
  } | null = null;
  /** Content Chrome refused to write (and the offer it came from), for the next gesture. */
  let unwritten: { serial: number; blobs: Map<LocalType, Blob> } | null = null;
  /** Offers from the server so far; a copy chord's write, waiting for the text of an offer that
   *  comes after the `offers` it saw. */
  let offers = 0;
  let copyWait: { after: number; resolve(text: Blob): void } | null = null;
  /** Whether the page may read the clipboard when the window gets focus: Chrome (once the
   *  user allowed it). Safari and Firefox do not know the `clipboard-read` permission, and
   *  read only in a paste event. Null until the browser answered. */
  let focusReads: boolean | null = null;
  const known = (
    navigator.permissions?.query({ name: "clipboard-read" as PermissionName }) ??
    Promise.reject()
  ).then(
    () => (focusReads = true),
    () => (focusReads = false),
  );
  /** The chords, as in `./input.ts`: Cmd on a Mac, Ctrl elsewhere. */
  const isMac = /Mac/.test(navigator.platform);

  // One clipboard operation at a time, in order: a read on focus must not run in the middle of
  // a write.
  let chain = Promise.resolve();
  const run = (f: () => Promise<void> | void) => {
    chain = chain.then(f).catch((e) => console.warn("clipboard:", e));
  };
  const focused = () => connected && document.hasFocus();

  /** Offer local content to the server, and keep it to answer its requests. */
  function offerLocal(blobs: Map<LocalType, Blob>) {
    const offer = { serial: Date.now(), mime_types: offerMimes([...blobs.keys()]) };
    owned = blobs;
    // This copy is newer than any offer not written yet, and than the reply on its way.
    setPending({ ...offer, own: true });
    unwritten = null;
    fetching = null;
    o.send({ k: "offer", ...offer });
  }

  /** On focus: offer the local clipboard if it is new, else fetch the pending offer. */
  async function syncLocal() {
    await known;
    if (!focused()) return;
    const blobs = focusReads ? await readLocal() : null;
    if (blobs?.size) {
      const hash = await digest(blobs);
      if (hash !== localStorage.getItem(SEEN_KEY)) {
        localStorage.setItem(SEEN_KEY, hash);
        offerLocal(blobs);
        return;
      }
    }
    fetchPending();
  }

  /** A paste chord's paste event (no focus reads): offer what it carries, before its V reaches
   *  the clone. The page's terminal pastes for itself. */
  function onPaste(e: ClipboardEvent) {
    if (focusReads !== false || !connected || !e.clipboardData) return;
    if (e.target instanceof HTMLTextAreaElement) return;
    e.preventDefault();
    const blobs = pasted(e.clipboardData);
    if (blobs.size) offerLocal(blobs);
  }

  /** A copy chord (no focus reads): write now, while the browser allows it, a text item whose
   *  content is the text of the clone's next offer. Only text: what else the clone copied is
   *  not known yet. With no new text in COPY_WAIT_MS the write fails and the clipboard stays as
   *  it was. */
  function onCopyKey(e: KeyboardEvent) {
    if (focusReads !== false || !connected || e.repeat || e.altKey) return;
    if (!(isMac ? e.metaKey : e.ctrlKey) || (e.code !== "KeyC" && e.code !== "KeyX")) return;
    if (e.target instanceof HTMLTextAreaElement) return;
    // This copy is newer than a write refused before: the next gesture must not write that.
    unwritten = null;
    const text = new Promise<Blob>((resolve, reject) => {
      const wait = { after: offers, resolve };
      copyWait = wait;
      setTimeout(() => {
        if (copyWait === wait) copyWait = null;
        reject(new Error("no new text from the clone"));
      }, COPY_WAIT_MS);
    });
    navigator.clipboard
      .write([new ClipboardItem({ "text/plain": text })])
      .catch((err) => console.warn("clipboard: copy chord not written:", err));
  }

  function fetchPending() {
    const p = pending();
    if (!p || p.own || !focused() || fetching?.serial === p.serial) return;
    const picks = pickMimes(p.mime_types);
    if (!picks.length) return;
    unwritten = null;
    fetching = { serial: p.serial, want: new Map(picks.map((x) => [x.mime, x.type])), got: new Map() };
    for (const x of picks) o.send({ k: "request", serial: p.serial, mime_type: x.mime });
  }

  async function write(serial: number, blobs: Map<LocalType, Blob>) {
    try {
      await navigator.clipboard.write([new ClipboardItem(Object.fromEntries(blobs))]);
    } catch {
      unwritten = { serial, blobs }; // no focus or no user activation: retry on the next gesture
      return;
    }
    if (pending()?.serial === serial) setPending(null);
    // Without focus reads nothing reads it back, and a read would raise Safari's paste prompt.
    if (!focusReads) return;
    // Remember what Chrome holds now (it re-encodes images and cleans HTML), so focusing a
    // window does not offer it back to the clone.
    const back = await readLocal();
    if (back?.size) localStorage.setItem(SEEN_KEY, await digest(back));
  }

  async function serve(serial: number, mime: string) {
    const type = localTypeFor(mime);
    // The serial is not checked: the server sends a request only to
    // the current owner, and a clone can ask under the serial of the offer before (its paste
    // raced the new offer).
    const blob = type ? owned?.get(type) : undefined;
    let bytes = new Uint8Array(blob ? await blob.arrayBuffer() : new ArrayBuffer(0));
    if (bytes.length > MAX_BYTES) bytes = new Uint8Array(0);
    o.sendData({ serial, mimeType: mime, bytes });
  }

  const onFocus = () => run(syncLocal);
  window.addEventListener("focus", onFocus);
  window.addEventListener("paste", onPaste);
  // Capture phase: before the input's key handler, whose gesture would write `unwritten`.
  window.addEventListener("keydown", onCopyKey, true);

  return {
    onMessage(msg) {
      if (msg.k === "offer") {
        const p = pending();
        if (p?.own && p.serial === msg.serial) return; // a sibling window's copy
        offers++;
        setPending({ serial: msg.serial, mime_types: msg.mime_types });
        // A new copy, even with the serial of the last one: serials restart per clone and when
        // the clone's clipboard holder restarts.
        fetching = null;
        run(fetchPending);
      } else if (msg.k === "request") {
        void serve(msg.serial, msg.mime_type);
      }
    },
    onData(d) {
      const f = fetching;
      const type = f?.serial === d.serial ? f.want.get(d.mimeType) : undefined;
      if (!f || !type || !d.bytes.length) return;
      // Write what came so far after each reply: one type that never comes (an image too
      // large, a clone read that hangs) must not hold back the others.
      f.got.set(type, new Blob([d.bytes], { type }));
      if (type === "text/plain" && copyWait && offers > copyWait.after) {
        copyWait.resolve(f.got.get(type)!);
        copyWait = null;
        f.byChord = true;
      }
      const blobs = new Map(f.got);
      // The chord's write has the text. The rest waits for the next gesture: a second write
      // now could cancel the chord's.
      if (f.byChord) {
        if (blobs.size > 1) unwritten = { serial: f.serial, blobs };
        return;
      }
      run(() => write(f.serial, blobs));
    },
    setConnected(c) {
      connected = c;
      fetching = null;
      if (c) run(syncLocal);
    },
    onGesture() {
      const u = unwritten;
      unwritten = null;
      if (!u) return;
      // Safari takes a write only when it is called inside the key press or click, not one
      // that waits behind other clipboard work.
      if (focusReads) run(() => write(u.serial, u.blobs));
      else void write(u.serial, u.blobs);
    },
    pasteByEvent: () => focusReads === false,
    detach() {
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("paste", onPaste);
      window.removeEventListener("keydown", onCopyKey, true);
    },
  };
}
