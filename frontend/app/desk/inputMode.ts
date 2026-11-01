// The /ng page's two modes, chosen by the input used last, so a laptop with a touch screen
// changes as its operator does (`./FullDesk.tsx`):
//
// - touch: the floating buttons, the on-screen keyboard, and the screen as a trackpad (or, with
//   the keyboard docked, as a touch screen).
// - mouse: the mouse and the keyboard go to the clone as in the viewer (`~/viewer/input.ts`); no
//   floating buttons; a shortcut opens the control panel (`./shortcut.ts`).
import { useEffect, useState } from "react";

export type InputMode = "touch" | "mouse";

/** The mode a page starts in: mouse where the main pointer is a fine one (a mouse, a
 *  trackpad), touch where it is a finger. */
export function startMode(): InputMode {
  return typeof window !== "undefined" && window.matchMedia("(pointer: fine)").matches ? "mouse" : "touch";
}

/** What an event says about the input in use. */
export type InputEvent =
  | { t: "pointerdown"; pointerType: string }
  | { t: "pointermove"; pointerType: string; moved: boolean }
  | { t: "keydown"; editable: boolean; repeat: boolean };

/** The mode after `e`; null: it says nothing. A finger or a pen is touch. A mouse that moves
 *  or clicks is mouse. A key is mouse (a keyboard), unless it types into a text box of the
 *  page: a phone's own keyboard does that too. */
export function modeAfter(e: InputEvent): InputMode | null {
  switch (e.t) {
    case "pointerdown":
      return e.pointerType === "mouse" ? "mouse" : "touch";
    case "pointermove":
      return e.pointerType === "mouse" && e.moved ? "mouse" : null;
    case "keydown":
      return e.editable || e.repeat ? null : "mouse";
  }
}

/** The target takes typed text itself. */
function editable(t: EventTarget | null): boolean {
  return t instanceof HTMLElement && (t.isContentEditable || t.matches("input, textarea, select"));
}

/** The mode now, following each input. */
export function useInputMode(): InputMode {
  const [mode, setMode] = useState<InputMode>(startMode);
  useEffect(() => {
    const set = (m: InputMode | null) => m && setMode(m);
    const onDown = (e: PointerEvent) => set(modeAfter({ t: "pointerdown", pointerType: e.pointerType }));
    const onMove = (e: PointerEvent) =>
      set(modeAfter({ t: "pointermove", pointerType: e.pointerType, moved: e.movementX !== 0 || e.movementY !== 0 }));
    const onKey = (e: KeyboardEvent) => set(modeAfter({ t: "keydown", editable: editable(e.target), repeat: e.repeat }));
    const opts = { capture: true, passive: true };
    window.addEventListener("pointerdown", onDown, opts);
    window.addEventListener("pointermove", onMove, opts);
    window.addEventListener("keydown", onKey, opts);
    return () => {
      window.removeEventListener("pointerdown", onDown, opts);
      window.removeEventListener("pointermove", onMove, opts);
      window.removeEventListener("keydown", onKey, opts);
    };
  }, []);
  return mode;
}
