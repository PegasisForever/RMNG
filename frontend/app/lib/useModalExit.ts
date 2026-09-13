// Delayed unmount for a dialog's exit frames. `ModalShell` is the only caller: this is the
// timing half of that module, split out so the frame's markup and the frame's clock can be
// read apart from each other.
//
// Entry plays free on mount, but React unmounts the moment a close flag flips, so the exit
// frames would never run. Instead every close intent goes through `beginExit`: it raises
// `closing` (the shell swaps its `-in` classes for the `-out` mirrors), waits out the
// animation, then runs the real close. Reduced-motion screens skip the wait — CSS already
// kills the frames, and a dead 220ms pause before anything happens would read as a hang.
import { useCallback, useEffect, useRef, useState } from "react";

/** Must cover the exit animation in app.css (`MODAL_ANIMATION_MS`) plus a frame of slack. */
export const MODAL_EXIT_MS = 220;

export function useModalExit() {
  const [closing, setClosing] = useState(false);
  const timer = useRef<number | null>(null);
  useEffect(
    () => () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    },
    [],
  );
  const beginExit = useCallback((onExited: () => void) => {
    // Already closing: a second Escape, or the settle-effect firing again, must not
    // stack another timer and close twice.
    if (timer.current !== null) return;
    if (
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches
    ) {
      onExited();
      return;
    }
    setClosing(true);
    timer.current = window.setTimeout(() => {
      timer.current = null;
      onExited();
    }, MODAL_EXIT_MS);
  }, []);
  return { closing, beginExit };
}
