// The two places the touch input (`./touchInput.ts`) goes: the clone's desktop, through the
// session (`./session.ts`), and a headless clone's terminal (`~/viewer/terminal.tsx`'s `TermApi`).
//
// The desktop gets the keys and buttons as they are, the text as X keysyms, and the pointer
// as the cursor's new place. The terminal gets the bytes the keys make (`./termKeys.ts`), and a
// pointer the page draws over it, whose clicks, drags and scrolls go to the terminal as a
// mouse's events, with the modifiers down: it selects, or the app that asked for the mouse gets
// them.
import type { InputMsg } from "~/lib/wire/InputMsg";
import { BTN_MIDDLE, BTN_RIGHT } from "~/viewer/keycodes";
import type { TermApi } from "~/viewer/terminal";

import { textToKeysyms } from "./dictation";
import { clampTo, pointerGain, scrollGain } from "./pointer";
import { TermKeys } from "./termKeys";
import type { CloneSink } from "./touchInput";

/** Mutter axis flags: a finger scroll, and its end. */
const AXIS_FINGER = 4;
const AXIS_FINISH = 1;

/** The part of the session the desktop's input uses. */
export interface DeskSession {
  send(msg: InputMsg): void;
}

/** The desktop's screen now, as the page shows it. */
export interface DeskView {
  /** The monitor shown: its id and size, monitor px. */
  monitor: number;
  width: number;
  height: number;
  /** Monitor px per screen px (CSS px), for the cursor moves. */
  px: number;
  /** The layout's scroll speed, percent. */
  scrollSpeed: number;
  /** The drawn cursor, monitor px. */
  cursor: { x: number; y: number };
  /** This page moved the cursor there. */
  moved(x: number, y: number): void;
}

/** The desktop of the clone, through `session` (null while there is none). */
export function desktopSink(session: () => DeskSession | null, view: () => DeskView): CloneSink {
  const send = (msg: InputMsg) => session()?.send(msg);
  return {
    desktop: true,
    key: (keycode, pressed) => send({ kind: "key_code", keycode, pressed }),
    button: (button, pressed) => send({ kind: "button", button, pressed }),
    text(text) {
      // Keysyms, so the clone's keyboard layout does not matter
      for (const keysym of textToKeysyms(text)) {
        send({ kind: "key", keysym, pressed: true });
        send({ kind: "key", keysym, pressed: false });
      }
    },
    move(dx, dy, ms) {
      // Screen px for this finger motion, then monitor px
      const v = view();
      const g = pointerGain(Math.hypot(dx, dy), ms) * v.px;
      const x = clampTo(v.cursor.x + dx * g, v.width);
      const y = clampTo(v.cursor.y + dy * g, v.height);
      v.moved(x, y);
      send({ kind: "pointer_move", monitor_id: v.monitor, x, y });
    },
    scroll(dx, dy) {
      const gain = scrollGain(view().scrollSpeed);
      send({ kind: "axis_continuous", dx: dx * gain, dy: dy * gain, flags: AXIS_FINGER });
    },
    scrollEnd: () => send({ kind: "axis_continuous", dx: 0, dy: 0, flags: AXIS_FINGER | AXIS_FINISH }),
  };
}

/** The terminal of a headless clone: `api` is its shown tab (null while there is none).
 *  `pointer` hears where its pointer is now, CSS px in the terminal. */
export function terminalSink(api: () => TermApi | null, pointer: (p: { x: number; y: number }) => void): CloneSink {
  const keys = new TermKeys();
  /** The pointer, and a mouse's `buttons` down; x < 0 until a trackpad moves it. */
  const p = { x: -1, y: -1, buttons: 0 };
  /** The pointer, put in the middle the first time, and kept inside the terminal. */
  const at = (dx = 0, dy = 0) => {
    const size = api()?.size();
    if (!size) return p;
    if (p.x < 0) [p.x, p.y] = [size.width / 2, size.height / 2];
    p.x = clampTo(p.x + dx, size.width);
    p.y = clampTo(p.y + dy, size.height);
    pointer({ x: p.x, y: p.y });
    return p;
  };
  return {
    desktop: false,
    key(code, pressed) {
      const t = api();
      const bytes = keys.key(code, pressed, t?.appCursor() ?? false);
      if (bytes) t?.input(bytes);
    },
    button(code, pressed) {
      const button = code === BTN_RIGHT ? 2 : code === BTN_MIDDLE ? 1 : 0;
      const bit = [1, 4, 2][button];
      at();
      p.buttons = pressed ? p.buttons | bit : p.buttons & ~bit;
      api()?.mouse(pressed ? "mousedown" : "mouseup", p.x, p.y, { button, buttons: p.buttons, ...keys.mods() });
    },
    text: (text) => api()?.input(text),
    move(dx, dy, ms) {
      const g = pointerGain(Math.hypot(dx, dy), ms);
      at(dx * g, dy * g);
      api()?.mouse("mousemove", p.x, p.y, { buttons: p.buttons, ...keys.mods() });
    },
    // The terminal's lines follow the fingers
    scroll(_dx, dy) {
      at();
      api()?.wheel(-dy, p.x, p.y);
    },
    scrollEnd() {},
  };
}
