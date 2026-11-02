// The page's keys, and the one place that receives them (`attachKeys`). The keys have one owner
// at a time, the clone or the page. The caller (`./remote.ts`) says only which, and gives the
// page's own actions; this module decides what each key does:
//
// - The clone has them in mouse and keyboard mode with nothing over the screen
//   (`./FullDesk.tsx`): each key goes to it as a Linux evdev keycode (`cloneKeyDown`). A key
//   typed into the page's terminal (xterm.js reads keys from a textarea) is the terminal's, and
//   where this window shows no desktop (a headless clone) no key goes to the clone.
// - The page has them otherwise (the control panel, a dialog, the right group, touch mode):
//   Escape, and on a Mac with the keyboard locked, the edit keys (`runEdit`).
// - In both: the control panel's chord (`isControlPanelChord`), and the Keyboard Lock: every key
//   is locked while the page is fullscreen, whoever has them, so Escape and the chord reach the
//   page instead of leaving fullscreen or going to the system.
//
// A change of owner lets go of every key the clone holds. The mouse's buttons are
// `~/viewer/input.ts`'s, which lets them go when it detaches.
import { hostWindow } from "~/lib/host";
import type { InputMsg } from "~/lib/wire/InputMsg";
import { evdevFor } from "~/viewer/keycodes";

/** A paste chord's key goes to the clone at most this long after its key press, also when no
 *  paste event comes (`CloneKeys.clipboard.pasteByEvent`). */
const PASTE_WAIT_MS = 100;
/** The keys Caps Lock changes: before one goes to the clone, its Caps Lock is brought in step. */
const LETTER = /^Key[A-Z]$/;
/** What has its own Escape: a dialog, a menu, an open list. */
const POPUPS = "[data-modal], [role=dialog], [role=menu], [role=listbox]";

// The chord

export type Os = "mac" | "windows" | "linux";

function currentOs(): Os {
  const p = typeof navigator === "undefined" ? "" : navigator.platform;
  return /Mac/.test(p) ? "mac" : /Win/.test(p) ? "windows" : "linux";
}

type Chord = Pick<KeyboardEvent, "code" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey">;

/** Whether `e` is the control panel's chord on `os`: Option+Space on macOS, Super+Space
 *  elsewhere. The system may keep it for itself (GNOME and Windows give Super+Space to the input
 *  source) and hand it to the page only in the page's own fullscreen, where the keys are locked
 *  (not the browser's fullscreen): it needs to work there only. */
export function isControlPanelChord(e: Chord, os: Os = currentOs()): boolean {
  if (e.code !== "Space" || e.ctrlKey || e.shiftKey) return false;
  return os === "mac" ? e.altKey && !e.metaKey : e.metaKey && !e.altKey;
}

/** The chord as the operator presses it on `os`, for the Control panel button's tooltip. Off a Mac
 *  the system keeps it outside the page's fullscreen, so the label says where it works. */
export function controlPanelChordLabel(os: Os = currentOs()): string {
  if (os === "mac") return "⌥ Option + Space";
  return `${os === "windows" ? "Windows" : "Super"} + Space (in fullscreen)`;
}

/** The keys (DOM codes) of the chord's modifier on `os`. Their press is held back from the clone
 *  until the next key or click: a Super or an Alt pressed and let go alone does something there
 *  (GNOME's overview, an app's menu bar), so the chord's must never reach it. */
function chordHolds(os: Os): readonly string[] {
  return os === "mac" ? ["AltLeft", "AltRight"] : ["MetaLeft", "MetaRight"];
}

// Fullscreen and the Keyboard Lock

/** Chrome's Keyboard Lock API, the browser window's (`~/lib/host.ts`); not in TypeScript's DOM
 *  types. */
const keyboard = () =>
  (hostWindow().navigator as Navigator & { keyboard?: { lock(keys?: string[]): Promise<void>; unlock(): void } })
    .keyboard;

/** The page's fullscreen and the Keyboard Lock (`attachKeys`); a test gives a fake. */
export interface Fullscreen {
  on(): boolean;
  toggle(): void;
  /** Lock every key (true), or give them back. */
  lock(on: boolean): void;
  /** Call `f` on each change of `on()`; returns the stop. */
  watch(f: () => void): () => void;
}

/** Fullscreen on or off, on the screen the window is on. The page's own fullscreen hides
 *  the browser's address bar, which macOS fullscreen (the green button) keeps. A browser
 *  without Keyboard Lock is asked for `keyboardLock: "browser"` (Safari 26.4, Firefox 156): for
 *  as long as the fullscreen lasts, Esc and the browser's own shortcuts go to the clone, and a
 *  held Esc leaves, as with Chrome's Keyboard Lock. A browser that does not know the option
 *  ignores it. In the shell's frame, the shell's page goes fullscreen, so a reload of the
 *  frame keeps it. */
export function toggleFullscreen() {
  const document = hostWindow().document;
  if (document.fullscreenElement) void document.exitFullscreen();
  else {
    const options = keyboard() ? undefined : { keyboardLock: "browser" };
    // Locked here too, not only when the keys see the change: they may be attached later
    void document.documentElement
      .requestFullscreen(options as FullscreenOptions | undefined)
      .then(() => keyboard()?.lock())
      .catch(() => {});
  }
}

/** The shell's fullscreen, or this page's own (`~/lib/host.ts`). */
const pageFullscreen: Fullscreen = {
  on: () => !!hostWindow().document.fullscreenElement,
  toggle: toggleFullscreen,
  lock(on) {
    if (on) void keyboard()?.lock();
    else keyboard()?.unlock();
  },
  watch(f) {
    const doc = hostWindow().document;
    doc.addEventListener("fullscreenchange", f);
    return () => doc.removeEventListener("fullscreenchange", f);
  },
};

// The edit keys
//
// On a Mac, Chrome runs Cmd+C, Cmd+X, Cmd+V, Cmd+A, Cmd+Z from its Edit menu, and a locked
// keyboard sends those keys to the page as plain keys instead: nothing copies or pastes. So
// while the page has the keys, it does each one itself, on the focused field or the selection:
//
// - copy and cut: the browser's own command, whose copy or cut event an editor (BlockNote) takes
//   as from the menu;
// - paste: the clipboard read (the frame may: `public/shell.html`), then, in an editor, a paste
//   event carrying it (text, HTML, images), and in a text box the text typed in;
// - select all: the browser's own command;
// - undo and redo: in a text box, the browser's own command (an editor has its own keys for both).
//
// A key the page already took (an editor's own Cmd+Z, Cmd+A) is left alone. Off a Mac the browser
// does all of these in the page.

type EditAction = "copy" | "cut" | "paste" | "selectAll" | "undo" | "redo";

/** The edit shortcut `e` is on a Mac (Cmd), or null. */
function editAction(e: Chord): EditAction | null {
  if (!e.metaKey || e.ctrlKey || e.altKey) return null;
  switch (e.code) {
    case "KeyC":
      return e.shiftKey ? null : "copy";
    case "KeyX":
      return e.shiftKey ? null : "cut";
    case "KeyV":
      return e.shiftKey ? null : "paste";
    case "KeyA":
      return e.shiftKey ? null : "selectAll";
    case "KeyZ":
      return e.shiftKey ? "redo" : "undo";
    default:
      return null;
  }
}

/** A text box (input, textarea) the keys type into. */
const isTextBox = (el: Element | null): el is HTMLInputElement | HTMLTextAreaElement =>
  el instanceof HTMLTextAreaElement || (el instanceof HTMLInputElement && !["checkbox", "radio", "button"].includes(el.type));

/** Paste the clipboard into the focused text box or editor. */
async function paste() {
  const target = document.activeElement;
  const editor = target instanceof HTMLElement && target.isContentEditable ? target : null;
  if (!isTextBox(target) && !editor) return;
  if (editor) {
    // Everything the clipboard holds that an editor takes: text, HTML, images
    const data = new DataTransfer();
    try {
      for (const item of await navigator.clipboard.read()) {
        for (const type of item.types) {
          const blob = await item.getType(type);
          if (type === "text/plain" || type === "text/html") data.setData(type, await blob.text());
          else if (type.startsWith("image/")) data.items.add(new File([blob], `pasted.${type.slice(6)}`, { type }));
        }
      }
    } catch {
      const text = await navigator.clipboard.readText().catch(() => "");
      if (text) data.setData("text/plain", text);
    }
    const event = new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true });
    // An editor that takes the paste cancels it; one that does not gets the text typed in
    if (editor.dispatchEvent(event)) document.execCommand("insertText", false, data.getData("text/plain"));
    return;
  }
  const text = await navigator.clipboard.readText().catch(() => "");
  if (text) document.execCommand("insertText", false, text);
}

/** Run the edit shortcut `e` is, if it is one. */
function runEdit(e: KeyboardEvent) {
  const action = editAction(e);
  if (!action) return;
  if ((action === "undo" || action === "redo") && !isTextBox(document.activeElement)) return;
  e.preventDefault();
  if (action === "paste") void paste();
  else document.execCommand(action);
}

// The modifiers the clone holds

/** The event flag that says whether a modifier key (by its DOM code) is down. */
type ModFlag = "ctrlKey" | "shiftKey" | "altKey" | "metaKey";

function modFlagOf(code: string): ModFlag | undefined {
  if (code === "ControlLeft" || code === "ControlRight") return "ctrlKey";
  if (code === "ShiftLeft" || code === "ShiftRight") return "shiftKey";
  if (code === "AltLeft" || code === "AltRight") return "altKey";
  if (code === "MetaLeft" || code === "MetaRight") return "metaKey";
  return undefined;
}

// The interface

/** What the page does with the keys it has. */
export interface PageKeys {
  /** The control panel's chord was pressed, whoever has the keys. */
  chord(): void;
  /** Escape that nothing in the page took, with no dialog or menu open (each has its own
   *  Escape). True: it did something, and the key goes no further. */
  escape(): boolean;
}

/** Where the keys go while the clone has them. */
export interface CloneKeys {
  /** Send one key (`key_code`) to the clone. */
  send(msg: InputMsg): void;
  /** Send this keyboard's Caps Lock (on or off), for the clone's to follow: the clone-daemon
   *  reads the clone's own and taps it when the two differ (`wire::socket::LockState`). */
  sendLocks(caps: boolean): void;
  /** The clipboard (`~/viewer/clipboard.ts`): told of each key that goes to the clone, before
   *  its gesture. `pasteByEvent`: the local clipboard reaches the page only in the paste event
   *  of a paste chord (Safari, Firefox); then the chord's key keeps its default action, which
   *  fires that event, and goes to the clone after it, behind the offer the event makes. */
  clipboard: { onCopyKey(e: KeyboardEvent): void; onGesture(): void; pasteByEvent(): boolean };
  /** This window's mouse in the clone (`~/viewer/input.ts`): whether it shows a monitor of the
   *  clone's desktop, and the pointer lock's chords (Ctrl+Alt+G: on or off; Ctrl+Alt+P: off). */
  mouse: { desktop(): boolean; pointerLock(chord: "toggle" | "off"): void };
}

export interface Keys {
  /** Who has the keys now: the clone, or the page (null). A change lets go of every key the
   *  clone holds. */
  give(clone: CloneKeys | null): void;
  detach(): void;
}

/** Take the window's keys (see the top of this file). `os` and `fullscreen` are the browser's
 *  unless a test gives its own. */
export function attachKeys(page: PageKeys, env: { os?: Os; fullscreen?: Fullscreen } = {}): Keys {
  const os = env.os ?? currentOs();
  const isMac = os === "mac";
  const fullscreen = env.fullscreen ?? pageFullscreen;
  const holds = chordHolds(os);
  /** The keys' owner: the clone, or (null) the page. */
  let clone: CloneKeys | null = null;
  /** The keys the clone holds from this window. */
  const keys = new Set<number>();
  /** The modifiers among `keys`, with the flag an event says them down by (`syncMods`). */
  const mods = new Map<number, ModFlag>();
  /** The chord's modifier's press, held back while the chord may follow (`chordHolds`). */
  let heldMod: number | null = null;
  /** A paste chord's key press, held until the paste event it fires has made its offer. */
  let heldPaste: { keycode: number; tap: boolean; timer: ReturnType<typeof setTimeout> } | null = null;
  /** Where a paste chord's paste goes. Safari pastes only into something editable, and fires
   *  no paste event otherwise, so the chord's key press moves the focus here, and the paste
   *  moves it back. Only for the chord: an editable focus would start an input method (IME)
   *  on every other key. */
  const pasteSink = document.createElement("div");
  pasteSink.contentEditable = "true";
  pasteSink.tabIndex = -1;
  pasteSink.ariaHidden = "true";
  pasteSink.style.cssText = "position:fixed;left:-100px;top:0;width:1px;height:1px;opacity:0";
  document.body.append(pasteSink);

  const send = (keycode: number, pressed: boolean) => clone?.send({ kind: "key_code", keycode, pressed });

  /** Let go of every key the clone holds from this window. */
  function release() {
    // A held paste key was never pressed in the clone.
    if (heldPaste) clearTimeout(heldPaste.timer);
    heldPaste = null;
    heldMod = null;
    pasteSink.blur();
    for (const k of keys) send(k, false);
    keys.clear();
    mods.clear();
  }

  /** Let go of the held modifiers `e` says are up. macOS takes Ctrl+Space for itself (the next
   *  input source) and Chrome gets no keyup for the Ctrl, so the clone would hold Ctrl until the
   *  page reloads. Every key, button, wheel and move event says which modifiers are down, so the
   *  next one lets go of the rest. */
  function syncMods(e: KeyboardEvent | MouseEvent) {
    // AltGr (right Alt) can come as "AltGraph" with `altKey` off: it is down all the same
    const down = { ctrlKey: e.ctrlKey, shiftKey: e.shiftKey, metaKey: e.metaKey, altKey: e.altKey || e.getModifierState("AltGraph") };
    for (const [k, flag] of mods) {
      if (down[flag]) continue;
      mods.delete(k);
      if (keys.delete(k)) send(k, false);
    }
  }

  /** Send the modifier's held press: another key or a click came, so it is no chord's. */
  function flushMod() {
    if (heldMod === null) return;
    keys.add(heldMod);
    send(heldMod, true);
    heldMod = null;
  }

  function onKeyDown(e: KeyboardEvent) {
    if (isControlPanelChord(e, os)) {
      e.preventDefault();
      if (e.repeat) return;
      // Its keys never reach the clone; what else is held is let go there
      release();
      page.chord();
      return;
    }
    if (clone) cloneKeyDown(clone, e);
    else pageKeyDown(e);
  }

  function pageKeyDown(e: KeyboardEvent) {
    if (e.defaultPrevented) return;
    if (e.key === "Escape") {
      if (!document.querySelector(POPUPS) && page.escape()) e.preventDefault();
      return;
    }
    // A Mac's edit keys while the keyboard is locked (see above)
    if (isMac && !e.repeat && fullscreen.on()) runEdit(e);
  }

  /** A key the page's own terminal reads (xterm.js reads keys from a textarea). */
  const forTerminal = (e: KeyboardEvent) => e.target instanceof HTMLTextAreaElement;

  function cloneKeyDown(c: CloneKeys, e: KeyboardEvent) {
    if (forTerminal(e)) return;
    syncMods(e);
    // No monitor (terminal mode): a key outside xterm must not reach the clone's desktop.
    if (!c.mouse.desktop()) return;
    // The copy chord's first: the gesture would write what the clipboard could not before
    c.clipboard.onCopyKey(e);
    c.clipboard.onGesture();
    // Local chords, not forwarded. Their Ctrl and Alt already went to the clone, so release
    // them (and everything else held) first.
    if (e.ctrlKey && e.altKey && (e.code === "KeyG" || e.code === "KeyP")) {
      e.preventDefault();
      if (e.repeat) return;
      release();
      c.mouse.pointerLock(e.code === "KeyG" ? "toggle" : "off");
      return;
    }
    if (e.code === "F11") {
      e.preventDefault();
      fullscreen.toggle();
      return;
    }
    if (e.code === "CapsLock") return capsSync(c, e);
    const k = evdevFor(e.code, isMac);
    if (k === undefined) return;
    // A letter: the clone's Caps Lock is this keyboard's before it lands
    if (!e.repeat && LETTER.test(e.code)) c.sendLocks(e.getModifierState("CapsLock"));
    const paste = e.code === "KeyV" && (isMac ? e.metaKey : e.ctrlKey) && !e.altKey && c.clipboard.pasteByEvent();
    if (!paste) e.preventDefault();
    const flag = modFlagOf(e.code);
    // macOS sends no keyup for a key released while Command is down, so the clone would hold
    // it (Cmd+V, sent as Ctrl+V, pasted again and again in the clone's key repeat). Such a key
    // goes as a tap: pressed and released at once, and again on each of the browser's repeats,
    // which stop when the key goes up.
    const tap = isMac && e.metaKey && !flag;
    if (tap && e.repeat) {
      if (!paste) {
        send(k, true);
        send(k, false);
      }
      return;
    }
    // The clone runs its own key repeat, so the browser's repeats are not sent.
    if (e.repeat) return;
    if (holds.includes(e.code) && !keys.has(k)) {
      heldMod = k;
      return;
    }
    flushMod();
    // Held already (pressed before Command went down, and let go with it down): press it again
    if (keys.has(k)) send(k, false);
    if (!tap) keys.add(k);
    if (flag) mods.set(k, flag);
    if (paste) {
      pasteSink.focus({ preventScroll: true });
      holdPaste(k, tap);
    } else {
      send(k, true);
      if (tap) send(k, false);
    }
  }

  function holdPaste(keycode: number, tap: boolean) {
    flushPaste();
    heldPaste = { keycode, tap, timer: setTimeout(flushPaste, PASTE_WAIT_MS) };
  }

  /** Send the held paste key's press (and, for a tap, its release). */
  function flushPaste() {
    if (!heldPaste) return;
    pasteSink.blur();
    clearTimeout(heldPaste.timer);
    send(heldPaste.keycode, true);
    if (heldPaste.tap) send(heldPaste.keycode, false);
    heldPaste = null;
  }

  // One task later, so every paste listener has run: the clipboard's sends the offer.
  const onPaste = () => {
    if (heldPaste) setTimeout(flushPaste, 0);
  };

  function onKeyUp(e: KeyboardEvent) {
    const c = clone;
    if (!c || forTerminal(e) || !c.mouse.desktop()) return;
    if (e.code === "CapsLock") return capsSync(c, e);
    const k = evdevFor(e.code, isMac);
    if (k === undefined) return;
    e.preventDefault();
    if (heldMod === k) {
      // The modifier alone: its tap goes to the clone now
      heldMod = null;
      send(k, true);
      send(k, false);
      return;
    }
    if (heldPaste?.keycode === k) flushPaste();
    mods.delete(k);
    if (!keys.delete(k)) return;
    send(k, false);
    // Chrome on macOS fires no keyup for a key released while Command is down (Cmd+C sends
    // only C's keydown), so Command's own release ends every key pressed with it.
    if (isMac && (e.code === "MetaLeft" || e.code === "MetaRight")) release();
  }

  /** Caps Lock is not sent as a key: a press only flips the clone's lock, and once the two
   *  disagree every press keeps them apart. Its state is sent instead, on its down and on its
   *  up (one of them may still say the old state: X11 reports the new one only on the up, and
   *  Chrome on macOS sends a down when the lock goes on and an up when it goes off), and the
   *  clone's follows it. */
  function capsSync(c: CloneKeys, e: KeyboardEvent) {
    e.preventDefault();
    c.sendLocks(e.getModifierState("CapsLock"));
  }

  // The mouse's events say which modifiers are down, and a click ends a held chord modifier.
  // Capture phase: before the mouse's own handlers send the move or the button.
  const onMouse = (e: MouseEvent) => {
    if (!clone) return;
    syncMods(e);
    if (e.type !== "pointermove") flushMod();
  };
  const onBlur = () => release();
  const onVisibility = () => {
    if (document.visibilityState === "hidden") release();
  };
  // Keyboard Lock only works in fullscreen, so take it on entry and give it back on exit.
  const unwatch = fullscreen.watch(() => fullscreen.lock(fullscreen.on()));
  // Attached while the page is fullscreen already: the keys are locked from now
  if (fullscreen.on()) fullscreen.lock(true);

  const passive = { capture: true, passive: true };
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("keyup", onKeyUp);
  window.addEventListener("pointermove", onMouse, passive);
  window.addEventListener("mousedown", onMouse, passive);
  window.addEventListener("wheel", onMouse, passive);
  window.addEventListener("paste", onPaste);
  window.addEventListener("blur", onBlur);
  document.addEventListener("visibilitychange", onVisibility);

  return {
    give(c) {
      if (c === clone) return;
      release();
      clone = c;
    },
    detach() {
      release();
      clone = null;
      unwatch();
      pasteSink.remove();
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("pointermove", onMouse, passive);
      window.removeEventListener("mousedown", onMouse, passive);
      window.removeEventListener("wheel", onMouse, passive);
      window.removeEventListener("paste", onPaste);
      window.removeEventListener("blur", onBlur);
      document.removeEventListener("visibilitychange", onVisibility);
      // The keyboard stays locked (every key) while the page is fullscreen
    },
  };
}
