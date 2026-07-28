// Escape-to-close for modal dialogs, stacked so only the TOPMOST open modal reacts.
//
// Clicking a modal's backdrop no longer closes it (a stray click outside must never discard
// a half-filled dialog), which leaves Escape as the only keyboard dismissal. That is fine
// until two modals are open at once — SettingsPanel (z-50) with GroupLoginModal (z-60) on
// top of it. With each modal owning a plain `window` keydown listener, one Escape fires
// BOTH and the operator loses the panel underneath as collateral.
//
// So the mounted modals keep a shared LIFO stack, and a modal handles Escape only when it
// is on top. The stack is module-level rather than context because modals mount from
// several unrelated places (the route, SettingsPanel) and threading a provider through all
// of them buys nothing here.
import { useEffect } from "react";

/** Mounted modals, oldest first. The last entry owns Escape. */
const stack: symbol[] = [];

/**
 * Whether the modal identified by `token` should act on an Escape press. Split out of the
 * listener so the stacking rule — the entire reason this module exists — is testable
 * without a DOM renderer.
 *
 * A modal that is on top but `enabled: false` returns false AND, by virtue of still being
 * on top, prevents the one beneath from seeing the key at all.
 */
export function ownsEscape(token: symbol, enabled: boolean): boolean {
  return stack[stack.length - 1] === token && enabled;
}

/** Test-only: push a token as if a modal mounted. */
export function __pushModal(token: symbol) {
  stack.push(token);
}

/** Test-only: pop a token as if a modal unmounted. */
export function __popModal(token: symbol) {
  const i = stack.lastIndexOf(token);
  if (i !== -1) stack.splice(i, 1);
}

/**
 * Close `onClose` when Escape is pressed, but only while this modal is the topmost one.
 *
 * `enabled` gates the *handling*, not the stack membership: a modal that suppresses Escape
 * while an operation is in flight (CloneModal's `busy`) must still hold its stack slot, or
 * Escape would fall through and close the dialog underneath it instead.
 */
export function useModalEscape(onClose: () => void, enabled = true) {
  useEffect(() => {
    const token = Symbol("modal");
    stack.push(token);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && ownsEscape(token, enabled)) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      const i = stack.lastIndexOf(token);
      if (i !== -1) stack.splice(i, 1);
    };
  }, [onClose, enabled]);
}

/** Test-only: drop any leftover stack entries between cases. */
export function __resetModalEscapeStack() {
  stack.length = 0;
}
