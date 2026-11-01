// Dictation for the on-screen keyboard: the browser's speech recognition (Web Speech API; on iOS it
// is the phone's own dictation) turns speech into text, and the text goes to the clone as
// typed characters (X keysyms, which Mutter types with the clone's own keymap).
//
// One tap on the key starts it, the next stops it; the phone also stops it by itself after a
// pause. The words come in pieces: a piece is shown while it is still changing (`onInterim`),
// and typed once the recognizer is sure of it (`onFinal`).

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
  onresult:
    | ((e: { resultIndex: number; results: ArrayLike<{ isFinal: boolean; 0: { transcript: string } }> }) => void)
    | null;
}
type RecognitionClass = new () => Recognition;

function recognitionClass(): RecognitionClass | null {
  if (typeof window === "undefined") return null;
  const w = window as unknown as { SpeechRecognition?: RecognitionClass; webkitSpeechRecognition?: RecognitionClass };
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

export class Dictation {
  private rec: Recognition | null = null;
  private state: DictationState;
  /** The last text typed, for the space before the next piece. */
  private typed = "";
  private clearError: ReturnType<typeof setTimeout> | undefined;

  constructor(
    private on: {
      state: (s: DictationState) => void;
      /** The words heard so far that are still changing; "" when there are none. */
      interim: (text: string) => void;
      /** Text to type now. */
      final: (text: string) => void;
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
      let interim = "";
      for (let i = e.resultIndex; i < e.results.length; i++) {
        const r = e.results[i];
        const text = r[0].transcript;
        if (!r.isFinal) interim += text;
        else if (text.trim()) {
          const piece = joinPiece(this.typed, text);
          this.typed = piece;
          this.on.final(piece);
        }
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
      this.rec = null;
      this.on.interim("");
      if (this.state !== "error") this.set("off");
    };
    this.rec = rec;
    this.typed = "";
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
