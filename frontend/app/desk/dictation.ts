// Dictation for the on-screen keyboard: the browser's speech recognition (Web Speech API; on iOS it
// is the phone's own dictation) turns speech into text, and the text goes to the clone as
// typed characters (X keysyms, which Mutter types with the clone's own keymap).
//
// One tap on the key starts it, the next stops it; the phone also stops it by itself after a
// pause. The words come in pieces: a piece is shown while it is still changing (`interim`),
// and typed once the recognizer is sure of it (`edit`, through `TakeText`, which also fixes the
// text where Chrome on Android changes a piece it said it was sure of).

/** What the dictation key shows. */
export type DictationState = "off" | "starting" | "listening" | "error" | "unsupported";

/** The part of the Web Speech API used here; not in TypeScript's DOM types. */
interface Recognition {
  continuous: boolean;
  interimResults: boolean;
  lang: string;
  start(): void;
  stop(): void;
  onstart: (() => void) | null;
  onend: (() => void) | null;
  onerror: ((e: { error: string }) => void) | null;
  onresult: ((e: { resultIndex: number; results: ArrayLike<RecognitionResult> }) => void) | null;
}
type RecognitionClass = new () => Recognition;

function recognitionClass(): RecognitionClass | null {
  if (typeof window === "undefined") return null;
  const w = window as unknown as {
    SpeechRecognition?: RecognitionClass;
    webkitSpeechRecognition?: RecognitionClass;
  };
  return w.SpeechRecognition ?? w.webkitSpeechRecognition ?? null;
}

/** X keysyms for the characters of `text`: Latin-1 as itself, a new line as Return, a tab as
 *  Tab, anything else as its Unicode keysym (0x1000000 + the code point). */
export function textToKeysyms(text: string): number[] {
  return [...text].map((c) => {
    const cp = c.codePointAt(0)!;
    if (c === "\n") return 0xff0d;
    if (c === "\t") return 0xff09;
    if ((cp >= 0x20 && cp <= 0x7e) || (cp >= 0xa0 && cp <= 0xff)) return cp;
    return 0x1000000 + cp;
  });
}

/** `next` as it goes after `typed` (the last text typed): a space between two pieces of
 *  speech, unless one of them has it already or `next` starts with punctuation. */
export function joinPiece(typed: string, next: string): string {
  const piece = next.replace(/\s+/g, " ");
  if (!typed || /\s$/.test(typed) || /^[\s.,!?;:]/.test(piece)) return piece;
  return ` ${piece}`;
}

/** One result of the recognizer: its words, and whether they are sure. */
export interface RecognitionResult {
  isFinal: boolean;
  0: { transcript: string };
}

/** What to send to make the clone's text the text heard: `erase` characters back, then `type`. */
export interface TextEdit {
  erase: number;
  type: string;
}

/** The text of one take (from a start to the end), and what to send for it.
 *
 *  Each result event gives the whole list of results of the take. The text heard is the sure
 *  (final) results of that list, joined; this keeps what it typed, and sends only the
 *  difference: the new characters, and Backspaces where the end of the text changed.
 *
 *  A browser that keeps to the spec adds sure results, and never changes them: the text only
 *  grows, and no Backspace goes. Chrome on Android (`growing`) does not: it gives the words of
 *  one utterance again and again as they grow, each time as a sure result, either at the same
 *  index or at a new one ("can you", "can you see", "can you see this"), and it can give the
 *  same words again or change the last words. There a sure result that grows the one before it,
 *  repeats it or changes only its end takes its place, and the end of the text can change. */
export class TakeText {
  /** The text typed in this take. */
  private typed: string[] = [];
  /** The characters of `typed` that do not change any more. */
  private fixed = 0;
  /** Text typed before the list of results began again (Android can clear the list). */
  private base = "";

  constructor(private growing: boolean) {}

  /** What to send for the results of this event. */
  update(results: ArrayLike<RecognitionResult>): TextEdit {
    const pieces = this.pieces(results);
    let { text, lastAt } = this.join(this.base, pieces);
    let target = [...text];
    // Android can change the case of a word it typed ("hello", then "Hello"): no new list
    const same = this.growing ? common(lower(this.typed), lower(target)) : common(this.typed, target);
    if (same < this.fixed) {
      // The list began again, or a sure result changed: keep all that was typed, and add to it
      this.base = this.typed.join("");
      ({ text, lastAt } = this.join(this.base, pieces));
      target = [...text];
    }
    const kept = common(this.typed, target);
    const edit = {
      erase: this.typed.length - kept,
      type: target.slice(kept).join(""),
    };
    this.typed = target;
    // A spec browser changes nothing it typed; on Android only the last utterance after its
    // first word can change
    this.fixed = this.growing ? lastAt : target.length;
    return edit;
  }

  /** The sure results, each with its words on one line. */
  private pieces(results: ArrayLike<RecognitionResult>): string[] {
    const out: string[] = [];
    for (let i = 0; i < results.length; i++) {
      const r = results[i];
      if (!r.isFinal) continue;
      const words = r[0].transcript.replace(/\s+/g, " ").trim();
      if (!words) continue;
      if (this.growing && out.length && replaces(out[out.length - 1], words)) out[out.length - 1] = words;
      else out.push(words);
    }
    return out;
  }

  /** `pieces` after `base`, and where the first word of the last piece ends (code points). */
  private join(base: string, pieces: string[]): { text: string; lastAt: number } {
    let text = base;
    let lastAt = [...base].length;
    for (const words of pieces) {
      const piece = joinPiece(text, words);
      const start = [...text].length + [...piece].length - [...words].length;
      lastAt = start + [...words.split(" ")[0]].length;
      text += piece;
    }
    return { text, lastAt };
  }
}

/** The number of characters at the start of `a` and `b` that are the same. */
function common(a: string[], b: string[]): number {
  let n = 0;
  while (n < a.length && n < b.length && a[n] === b[n]) n++;
  return n;
}

/** Each character in lower case (one each, so the places stay). */
function lower(chars: string[]): string[] {
  return chars.map((c) => (c.toLowerCase().length === c.length ? c.toLowerCase() : c));
}

/** The words of a result, for a compare: lower case, no punctuation. */
function wordsOf(text: string): string[] {
  return text
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s']/gu, "")
    .split(/\s+/)
    .filter(Boolean);
}

/** On Android: `next` is the same utterance as `last`, said again, grown, or with a changed
 *  end. It has the same first word, at least as many words, and all the words of `last` in
 *  their order, but for a quarter of them (at least one) when `last` has three words or more.
 *  Two utterances of one word that are the same
 *  ("yes", "yes") are one; a spec browser does not use this. */
export function replaces(last: string, next: string): boolean {
  const a = wordsOf(last);
  const b = wordsOf(next);
  if (!a.length || !b.length || a[0] !== b[0] || b.length < a.length) return false;
  let kept = 0;
  for (const w of b) if (kept < a.length && w === a[kept]) kept++;
  // A short utterance only grows or comes again: "the cat" then "the dog" are two
  const may = a.length >= 3 ? Math.max(1, Math.floor(a.length / 4)) : 0;
  return a.length - kept <= may;
}

/** Chrome on Android, and the browsers made from it, give growing results (`TakeText`). */
function isAndroid(): boolean {
  return typeof navigator !== "undefined" && /Android/i.test(navigator.userAgent);
}

export class Dictation {
  private rec: Recognition | null = null;
  private state: DictationState;
  /** The text of this take. */
  private take: TakeText | null = null;
  private clearError: ReturnType<typeof setTimeout> | undefined;

  constructor(
    private on: {
      state: (s: DictationState) => void;
      /** The words heard so far that are still changing; "" when there are none. */
      interim: (text: string) => void;
      /** Change the text typed: characters back, then new text. */
      edit: (e: TextEdit) => void;
    },
  ) {
    this.state = recognitionClass() ? "off" : "unsupported";
  }

  get current(): DictationState {
    return this.state;
  }

  /** Start, or stop when it listens. Call it from inside a tap: the browser asks for that. */
  toggle() {
    if (this.state === "unsupported") return;
    if (this.state === "listening" || this.state === "starting") {
      this.rec?.stop();
      return;
    }
    this.start();
  }

  private start() {
    const Rec = recognitionClass()!;
    const rec = new Rec();
    rec.continuous = true;
    rec.interimResults = true;
    rec.lang = navigator.language || "en-US";
    rec.onstart = () => this.set("listening");
    rec.onresult = (e) => {
      if (rec !== this.rec) return;
      const edit = this.take!.update(e.results);
      if (edit.erase || edit.type) this.on.edit(edit);
      let interim = "";
      for (let i = e.resultIndex; i < e.results.length; i++) {
        const r = e.results[i];
        if (!r.isFinal) interim += r[0].transcript;
      }
      this.on.interim(interim.trim());
    };
    rec.onerror = (e) => {
      // Silence and a stop are no faults: it just ends
      if (e.error === "no-speech" || e.error === "aborted") return;
      this.set("error");
      clearTimeout(this.clearError);
      this.clearError = setTimeout(() => this.state === "error" && this.set("off"), 2000);
    };
    rec.onend = () => {
      if (rec !== this.rec) return;
      this.rec = null;
      this.on.interim("");
      if (this.state !== "error") this.set("off");
    };
    this.rec = rec;
    this.take = new TakeText(isAndroid());
    this.set("starting");
    try {
      rec.start();
    } catch {
      this.rec = null;
      this.set("error");
    }
  }

  /** Stop listening (the page goes away). */
  stop() {
    this.rec?.stop();
    clearTimeout(this.clearError);
  }

  private set(s: DictationState) {
    this.state = s;
    this.on.state(s);
  }
}
