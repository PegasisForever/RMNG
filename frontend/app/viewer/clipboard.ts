// The page's clipboard: each monitor window is one endpoint of the server's clipboard
// broker (docs/PROTOCOL.md "Clipboard (rich + lazy)").
//
// Chrome lets a page use the clipboard only while the page has focus, and it has no "clipboard
// changed" event. So:
// - Local → clone: when the window gets focus, read the local clipboard. If it changed since a
//   monitor window last saw it, offer it to the server, keep the bytes, and answer the server's
//   requests from them. A copy in the page itself (the side panels, the control panel) comes
//   with no change of focus, so each copy or cut in the page does the same once it is done.
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
// The on-screen keyboard has two keys for this, which work in every browser: one writes the
// clone's clipboard to the local one (`toLocal`), the other offers the local clipboard to the
// clone and gives its text, for the page to type in (`toClone`). Each starts in its tap, the only time Safari lets a page use the clipboard.
//
// All monitor windows share two values in localStorage: a hash of the last local content seen
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
/** How long `toLocal`'s write waits for the clone's content before it gives up. */
const TO_LOCAL_WAIT_MS = 10_000;

/** Most bytes sent per type: what still fits the 32 MiB clipboard limit once it is base64 on
 *  the server's hop to the clone. A larger copy is answered with no bytes (the paste fails). */
const MAX_BYTES = 23 * 1024 * 1024;

const SEEN_KEY = "rmng.clipboard.seen";
const PENDING_KEY = "rmng.clipboard.pending";

/** The shared pending offer: one from another endpoint that no window has written yet, or
 *  (`own`) the one a monitor window made. */
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

/** What `toLocal` writes of a clone's offer: one write fails as a whole when one type does not
 *  come, so only the types that belong together. Text (with its HTML, for a paste with its
 *  look), else the PNG image, else the HTML. */
export function pickForWrite(mimes: string[]): { mime: string; type: LocalType }[] {
  const all = pickMimes(mimes);
  const text = all.filter((x) => x.type !== "image/png");
  if (text.some((x) => x.type === "text/plain")) return text;
  const image = all.filter((x) => x.type === "image/png");
  return image.length ? image : text;
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
function htmlToText(html: string): string {
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

/** A text field of the page: what is pasted or copied there is the page's own. */
const editable = (t: EventTarget | null) =>
  t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || (t instanceof HTMLElement && t.isContentEditable);

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

interface PageClipboard {
  /** An offer or a request from the server. */
  onMessage(msg: ClipboardMsg): void;
  /** Bytes for a request this window made. */
  onData(d: ClipboardBytes): void;
  setConnected(connected: boolean): void;
  /** A key press or click: retry a write the browser refused. */
  onGesture(): void;
  /** Whether the local clipboard comes only from a paste chord's paste event. */
  pasteByEvent(): boolean;
  /** In a tap or a click: write the clone's clipboard to the local one. "same": it is the local
   *  clipboard's own content. Fails when the clone's clipboard is empty or does not come. */
  toLocal(): Promise<"written" | "same">;
  /** In a tap or a click: offer the local clipboard to the clone (Safari first shows its Paste
   *  button), and give its plain text (null: it has none, as an image). Null: there is nothing
   *  to offer. Fails when the browser refuses the read. */
  toClone(): Promise<{ text: string | null } | null>;
  detach(): void;
}

export function attachClipboard(o: ClipboardOptions): PageClipboard {
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
    /** `toLocal`'s write takes the types as they come; nothing else writes them. */
    waiters?: Map<LocalType, (b: Blob) => void>;
  } | null = null;
  /** The clone's clipboard: the offer the server sent last, or this window's own. `own`: this
   *  browser's content (this window's offer, or a sibling's). */
  let current: (ClipboardOffer & { own: boolean }) | null = null;
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
    current = { ...offer, own: true };
    // This copy is newer than any offer not written yet, and than the reply on its way.
    setPending({ ...offer, own: true });
    unwritten = null;
    fetching = null;
    o.send({ k: "offer", ...offer });
  }

  /** On focus: offer the local clipboard if it is new, else fetch the pending offer. */
  async function syncLocal(copied = false) {
    await known;
    if (!focused()) return;
    const blobs = focusReads ? await readLocal() : null;
    if (blobs?.size) {
      const hash = await digest(blobs);
      // A copy in the page is offered even when it is what the clipboard held before: it is
      // newer than what the clone copied since
      if (copied || hash !== localStorage.getItem(SEEN_KEY)) {
        localStorage.setItem(SEEN_KEY, hash);
        offerLocal(blobs);
        return;
      }
    }
    if (!copied) fetchPending();
  }

  /** A copy or a cut in the page: offer what it put on the clipboard. With focus reads, from
   *  the clipboard once the copy is done; else from what the copy carries (what the page's
   *  editor set on it), or the selection the browser copies itself. */
  function onCopy(e: ClipboardEvent) {
    if (!connected) return;
    if (focusReads) {
      setTimeout(() => run(() => syncLocal(true)), 0);
      return;
    }
    const blobs = e.clipboardData ? pasted(e.clipboardData) : new Map<LocalType, Blob>();
    const text = blobs.size ? "" : (document.getSelection()?.toString() ?? "");
    if (text) blobs.set("text/plain", new Blob([text], { type: "text/plain" }));
    if (blobs.size) offerLocal(blobs);
  }

  /** A paste chord's paste event (no focus reads): offer what it carries, before its V reaches
   *  the clone. The page's terminal pastes for itself. */
  function onPaste(e: ClipboardEvent) {
    if (focusReads !== false || !connected || !e.clipboardData) return;
    if (editable(e.target)) return;
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
    if (editable(e.target)) return;
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
    await written(serial);
  }

  /** After a write of the clone's content: it is no longer pending, and Chrome's copy of it is
   *  known as seen. */
  async function written(serial: number) {
    if (pending()?.serial === serial) setPending(null);
    // Without focus reads nothing reads it back, and a read would raise Safari's paste prompt.
    if (!focusReads) return;
    // Remember what Chrome holds now (it re-encodes images and cleans HTML), so focusing a
    // window does not offer it back to the clone.
    const back = await readLocal();
    if (back?.size) localStorage.setItem(SEEN_KEY, await digest(back));
  }

  function toLocal(): Promise<"written" | "same"> {
    const c = current;
    if (!connected) return Promise.reject(new Error("not connected"));
    if (!c) return Promise.reject(new Error("the clone's clipboard is empty"));
    if (c.own) {
      // The local clipboard's own content: this window still has it, or a sibling's copy is
      // in the local clipboard already
      if (!owned) return Promise.resolve("same");
      return navigator.clipboard.write([new ClipboardItem(Object.fromEntries(owned))]).then(() => "same");
    }
    const picks = pickForWrite(c.mime_types);
    if (!picks.length) return Promise.reject(new Error("the clone's clipboard has nothing this browser can take"));
    // The fetch of this offer that runs already (a focused window asks at once), or a new one
    if (fetching?.serial !== c.serial) fetching = { serial: c.serial, want: new Map(), got: new Map() };
    const f = fetching;
    const waiters = (f.waiters ??= new Map());
    unwritten = null;
    const items = Object.fromEntries(
      picks.map((x) => {
        const got = f.got.get(x.type);
        if (got) return [x.type, got];
        if (![...f.want.values()].includes(x.type)) {
          f.want.set(x.mime, x.type);
          o.send({ k: "request", serial: c.serial, mime_type: x.mime });
        }
        return [
          x.type,
          new Promise<Blob>((resolve, reject) => {
            waiters.set(x.type, resolve);
            setTimeout(() => reject(new Error("the clone's clipboard did not come")), TO_LOCAL_WAIT_MS);
          }),
        ];
      }),
    );
    // Called now, in the tap; the content comes later
    return navigator.clipboard.write([new ClipboardItem(items)]).then(async () => {
      await written(c.serial);
      return "written" as const;
    });
  }

  async function toClone(): Promise<{ text: string | null } | null> {
    if (!connected) throw new Error("not connected");
    // The read first, while the tap lets the page read
    const blobs = await readLocal();
    if (!blobs) throw new Error("the browser did not let the page read the clipboard");
    if (!blobs.size) return null;
    localStorage.setItem(SEEN_KEY, await digest(blobs));
    offerLocal(blobs);
    return { text: (await blobs.get("text/plain")?.text()) ?? null };
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

  const onFocus = () => run(() => syncLocal());
  window.addEventListener("focus", onFocus);
  window.addEventListener("paste", onPaste);
  window.addEventListener("copy", onCopy);
  window.addEventListener("cut", onCopy);
  // Capture phase: before the input's key handler, whose gesture would write `unwritten`.
  window.addEventListener("keydown", onCopyKey, true);

  return {
    onMessage(msg) {
      if (msg.k === "offer") {
        const p = pending();
        const own = !!p?.own && p.serial === msg.serial;
        current = { serial: msg.serial, mime_types: msg.mime_types, own };
        if (own) return; // a sibling window's copy
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
      if (f.waiters) {
        f.waiters.get(type)?.(f.got.get(type)!);
        return;
      }
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
      if (c) run(() => syncLocal());
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
    toLocal,
    toClone,
    detach() {
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("paste", onPaste);
      window.removeEventListener("copy", onCopy);
      window.removeEventListener("cut", onCopy);
      window.removeEventListener("keydown", onCopyKey, true);
    },
  };
}
