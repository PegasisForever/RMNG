// The touch input: what the on-screen keyboard's touches and the trackpad's (the screen while
// the keyboard floats) do to the clone. The touches become keys and pointer gestures
// (`./keypad.ts`); here they become what goes to the clone, through a `CloneSink` (`./cloneSink.ts`:
// the desktop's session, or a headless clone's terminal). This keeps the modifiers
// (`./modifiers.ts`), Fn, Caps Lock, the clipboard keys, dictation's text and the three-finger
// swipes. Ctrl, then a key or a click, is Ctrl with that key or a Ctrl-click, on either sink.
//
// No React: the page reads `state()` (a new object each time it changes) and `subscribe`s to
// its changes. The time and the waits come from the `Clock` given, so tests run them by hand.
import { BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, CODE_TO_EVDEV } from "~/viewer/keycodes";

import { Dictation, type DictationState } from "./dictation";
import { CAPS_LOCK, type Key, isShift } from "./keyboard";
import { Keypad, type KeypadOut, type Timer } from "./keypad";
import { type ModState, Modifiers } from "./modifiers";

/** Where the touch input goes: the clone's desktop or its terminal (`./cloneSink.ts`). */
export interface CloneSink {
  /** A key (evdev code) goes down or up. */
  key(code: number, pressed: boolean): void;
  /** A mouse button (evdev `BTN_*`) goes down or up, where the pointer is. */
  button(code: number, pressed: boolean): void;
  /** Type `text` as it is, whatever the clone's keyboard layout. */
  text(text: string): void;
  /** One finger moved the pointer: CSS px of finger motion, and the time it took, ms. */
  move(dx: number, dy: number, ms: number): void;
  /** Fingers scroll, CSS px (their mean); then they lift. */
  scroll(dx: number, dy: number): void;
  scrollEnd(): void;
  /** A desktop: three-finger swipes send their keys, and a pinch zooms the view. */
  readonly desktop: boolean;
}

/** The page's time and waits. */
export interface Clock extends Timer {
  now(): number;
}

/** What a finger did: as `./KeyboardView.tsx` says it. */
export type TouchKind = "down" | "move" | "up" | "cancel";

/** What the keys show, and what is said over the screen. */
export type TouchState = {
  /** The modifiers that are down (evdev code → held, tapped, or locked). */
  mods: ReadonlyMap<number, ModState>;
  fn: boolean;
  caps: boolean;
  /** The keys show as a trackpad (`./keypad.ts`: after a pointer gesture, for a while). */
  pad: boolean;
  dictation: DictationState;
  /** The words dictation still recognizes. */
  heard: string;
  /** What the last clipboard key did, for a while. */
  clipNote: string | null;
};

/** The part of the clipboard the clipboard keys use (`~/viewer/clipboard.ts`). */
export interface ClipKeys {
  /** Write the clone's clipboard to this device's ("same": it had it already). */
  toLocal(): Promise<"written" | "same">;
  /** Offer this device's clipboard to the clone, and give its text; null: nothing to offer. */
  toClone(): Promise<{ text: string | null } | null>;
}

/** The part of dictation the key uses (`./dictation.ts`). */
export type DictationKey = Pick<Dictation, "toggle" | "stop" | "current">;
export type DictationEvents = ConstructorParameters<typeof Dictation>[0];

export interface TouchInputOptions {
  /** Where the input goes now. */
  sink: () => CloneSink;
  clock: Clock;
  /** The session's clipboard, null while there is none. */
  clipboard: () => ClipKeys | null;
  /** Makes the dictation (the browser's speech recognition by default). */
  dictation?: (on: DictationEvents) => DictationKey;
  /** A finger came down on a key: its click sound. */
  press?: (key: Key) => void;
  /** A pinch on the trackpad: the view's zoom changes by `factor`. */
  zoom?: (factor: number) => void;
}

/** How long the note of a clipboard key stays. */
export const CLIP_NOTE_MS = 4000;
/** The most characters the clipboard key types into the clone: a key press each, so a longer
 *  text is only offered, for a paste. */
export const TYPE_MAX_CHARS = 10_000;

export class TouchInput {
  private snap: TouchState;
  private listeners = new Set<() => void>();
  private mods: Modifiers;
  private keys: Keypad;
  private trackpad: Keypad;
  private dictate: DictationKey;
  private cancelNote = () => {};

  constructor(private o: TouchInputOptions) {
    this.mods = new Modifiers((code, pressed) => this.sink.key(code, pressed), isShift);
    this.keys = new Keypad((k) => (k.t === "pad" ? this.set({ pad: k.on }) : this.out(k)), o.clock);
    // A trackpad with no keys, for good, where two fingers also pinch to zoom the view
    this.trackpad = new Keypad((k) => this.out(k), o.clock, { alwaysPad: true, pinch: true });
    this.dictate = (o.dictation ?? ((on) => new Dictation(on)))({
      state: (dictation) => this.set({ dictation }),
      interim: (heard) => this.set({ heard }),
      final: (text) => this.sink.text(text),
    });
    this.snap = {
      mods: new Map(),
      fn: false,
      caps: false,
      pad: false,
      dictation: this.dictate.current,
      heard: "",
      clipNote: null,
    };
  }

  /** A finger on the keys: `key` is the key it went down on (null: no key there). */
  keyTouch = (kind: TouchKind, id: number, key: Key | null, x: number, y: number) =>
    this.feed(this.keys, kind, id, key, x, y);
  /** A finger on the trackpad. */
  padTouch = (kind: TouchKind, id: number, x: number, y: number) => this.feed(this.trackpad, kind, id, null, x, y);

  state = (): TouchState => this.snap;
  subscribe = (f: () => void) => {
    this.listeners.add(f);
    return () => {
      this.listeners.delete(f);
    };
  };

  /** The page goes away: dictation stops, the note's wait too. */
  stop() {
    this.dictate.stop();
    this.cancelNote();
  }

  private get sink() {
    return this.o.sink();
  }

  private set(patch: Partial<TouchState>) {
    this.snap = { ...this.snap, ...patch };
    for (const f of this.listeners) f();
  }

  private feed(pad: Keypad, kind: TouchKind, id: number, key: Key | null, x: number, y: number) {
    const t = this.o.clock.now();
    if (kind === "down") pad.down(id, key, x, y, t);
    else if (kind === "move") pad.move(id, x, y, t);
    else if (kind === "up") pad.up(id, t);
    else pad.cancel(id);
  }

  /** Run a change to the modifiers, then show their new states. */
  private withMods(f: (m: Modifiers) => void) {
    f(this.mods);
    this.set({ mods: this.mods.states() });
  }

  /** A key that is no modifier goes down or up; a tapped modifier has done its key after it. */
  private keyCode(code: number, pressed: boolean) {
    if (pressed) {
      if (code === CAPS_LOCK) this.set({ caps: !this.snap.caps });
      this.mods.keyDown();
    }
    this.sink.key(code, pressed);
    if (!pressed) this.withMods((m) => m.keyDone());
  }

  /** A mouse button, which counts as a key to the modifiers: Ctrl, then a click, is a
   *  Ctrl-click. */
  private button(code: number, pressed: boolean) {
    if (pressed) this.mods.keyDown();
    this.sink.button(code, pressed);
    if (!pressed) this.withMods((m) => m.keyDone());
  }

  /** Press the keys `codes` (DOM codes) in order, then let them go in the other order. */
  private chord(codes: string[]) {
    const keys = codes.map((c) => CODE_TO_EVDEV[c]);
    for (const code of keys) this.sink.key(code, true);
    for (const code of [...keys].reverse()) this.sink.key(code, false);
  }

  /** Say what a clipboard key did, for CLIP_NOTE_MS. */
  private sayClip(text: string) {
    this.cancelNote();
    this.set({ clipNote: text });
    this.cancelNote = this.o.clock.after(CLIP_NOTE_MS, () => this.set({ clipNote: null }));
  }

  /** A clipboard key's tap. The browser lets the page use the clipboard only in the tap, so the
   *  read or the write starts here and now. */
  private clipKey(way: NonNullable<Key["clip"]>) {
    const clip = this.o.clipboard();
    if (!clip) return this.sayClip("Not connected to the clone.");
    if (way === "toLocal") {
      clip.toLocal().then(
        (r) => this.sayClip(r === "same" ? "This device's clipboard has it already." : "Copied the clone's clipboard here."),
        (e: Error) => this.sayClip(`Not copied: ${e.message}.`),
      );
    } else {
      // The clipboard goes to the clone's clipboard, and its text is typed in
      clip.toClone().then(
        (r) => {
          if (!r) return this.sayClip("This device's clipboard is empty.");
          const text = r.text?.replace(/\r\n?/g, "\n") ?? "";
          if (!text) return this.sayClip("Copied this clipboard to the clone. Paste it there with Ctrl+V.");
          if (text.length > TYPE_MAX_CHARS) {
            return this.sayClip("Copied this clipboard to the clone. It is too long to type: paste it there with Ctrl+V.");
          }
          this.sink.text(text);
          this.sayClip("Typed this clipboard into the clone.");
        },
        (e: Error) => this.sayClip(`Not copied: ${e.message}.`),
      );
    }
  }

  /** What both keypads do, the keys' and the trackpad's. */
  private out(o: KeypadOut) {
    switch (o.t) {
      case "press":
        this.o.press?.(o.key);
        break;
      case "tap":
        if (o.key.fn) this.set({ fn: !this.snap.fn });
        else if (o.key.dictate) this.dictate.toggle();
        else if (o.key.clip) this.clipKey(o.key.clip);
        else {
          this.keyCode(o.key.code, true);
          this.keyCode(o.key.code, false);
        }
        break;
      case "click": {
        // As from a mouse button: a tapped modifier makes it a Ctrl-click
        const code = o.button === "right" ? BTN_RIGHT : o.button === "middle" ? BTN_MIDDLE : BTN_LEFT;
        this.button(code, true);
        this.button(code, false);
        break;
      }
      case "drag":
        // Tap, then touch and move: the left button is down until the finger lifts
        this.button(BTN_LEFT, o.pressed);
        break;
      case "mod": {
        const { code } = o.key;
        const t = this.o.clock.now();
        this.withMods((m) => (o.down ? m.down(code, t) : m.up(code, o.dragged ?? false, t)));
        break;
      }
      case "hold":
        this.keyCode(o.key.code, o.pressed);
        break;
      case "move":
        this.sink.move(o.dx, o.dy, o.ms);
        break;
      case "scroll":
        this.sink.scroll(o.dx, o.dy);
        break;
      case "scroll_end":
        this.sink.scrollEnd();
        break;
      case "swipe":
        if (!this.sink.desktop) break;
        // Up or down: the Win key (GNOME's overview); left or right: Ctrl+Alt+arrow (the
        // workspace beside), the other way, as the content follows the fingers: a swipe to the
        // left brings in the workspace on the right. Chords of their own, past the modifiers.
        this.chord(
          o.dir === "up" || o.dir === "down"
            ? ["MetaLeft"]
            : ["ControlLeft", "AltLeft", o.dir === "left" ? "ArrowRight" : "ArrowLeft"],
        );
        break;
      case "zoom":
        if (this.sink.desktop) this.o.zoom?.(o.factor);
        break;
      case "pad":
        break;
    }
  }
}
