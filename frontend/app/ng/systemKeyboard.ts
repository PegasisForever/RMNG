// The phone's own keyboard, for the text boxes on the /ng tab control panel. While it is up, the whole
// /ng screen moves up (`useLift`) so the open page is all above it, not only the text box:
// the top of the clone's screen goes out of view for that time. iOS tells how much of the page
// the keyboard leaves in view through `visualViewport`.
import { useEffect, useState } from "react";

/** A text box (or another field that brings up the phone's keyboard) has the focus.
 *  Checkboxes and buttons do not count. */
export function isTyping(): boolean {
  const e = document.activeElement;
  return (
    e instanceof HTMLElement &&
    (e.isContentEditable ||
      e.matches("textarea, select, input:not([type=checkbox], [type=radio], [type=button], [type=submit])"))
  );
}

/** How far (CSS px) to move the screen up so a page whose bottom edge is at `bottom` (page
 *  coordinates) is all in the part of the page in view, `top` to `top + height`. */
export function liftFor(bottom: number, top: number, height: number): number {
  return Math.max(0, Math.round(bottom - (top + height)));
}

type View = { typing: boolean; top: number; height: number };

function look(): View {
  const vv = window.visualViewport;
  return { typing: isTyping(), top: vv?.offsetTop ?? 0, height: vv?.height ?? window.innerHeight };
}

/** How far to move the screen up now, so that the bottom edge `bottom` (page coordinates) is
 *  above the phone's keyboard: 0 while no text box has the focus. */
export function useLift(bottom: number): number {
  const [view, setView] = useState<View | null>(null);
  useEffect(() => {
    const vv = window.visualViewport;
    const update = () => setView(look());
    // The focus can go from one text box to the next: look once it has arrived
    let later: ReturnType<typeof setTimeout> | undefined;
    const onFocusOut = () => {
      clearTimeout(later);
      later = setTimeout(() => {
        update();
        // iOS can leave the page scrolled by the keyboard after it went down
        if (!isTyping()) window.scrollTo(0, 0);
      }, 0);
    };
    document.addEventListener("focusin", update);
    document.addEventListener("focusout", onFocusOut);
    vv?.addEventListener("resize", update);
    vv?.addEventListener("scroll", update);
    return () => {
      clearTimeout(later);
      document.removeEventListener("focusin", update);
      document.removeEventListener("focusout", onFocusOut);
      vv?.removeEventListener("resize", update);
      vv?.removeEventListener("scroll", update);
    };
  }, []);
  return view?.typing ? liftFor(bottom, view.top, view.height) : 0;
}
