import { useSyncExternalStore } from "react";

/** Below this width the phone UI takes over. 768px is the usual phone-to-tablet line, so a
 *  tablet in portrait still gets the board.
 *
 *  That band, 768px up to the `lg` line, is the tight one: the side panel floats over the
 *  board taking 30% and never less than 320px, so the columns underneath keep only what is
 *  left. It stays usable because the panel floats rather than displacing anything. */
const MOBILE_MAX_WIDTH = 767;

const QUERY = `(max-width: ${MOBILE_MAX_WIDTH}px)`;

function subscribe(onChange: () => void): () => void {
  const mql = window.matchMedia(QUERY);
  mql.addEventListener("change", onChange);
  return () => mql.removeEventListener("change", onChange);
}

/** Whether the phone UI should render.
 *
 *  The switch is made in JavaScript and picks between two component trees, rather than in
 *  CSS across one tree. A phone is not a narrow desktop: the two shells share their data
 *  and almost none of their markup, so the board, its drag handlers, and the modals behind
 *  it are never built at all on a phone, and neither shell has to carry breakpoints for a
 *  layout it does not have.
 *
 *  `useSyncExternalStore` is what keeps that honest through hydration. The build-time
 *  prerender has no viewport, so the server snapshot answers `false`, and the client
 *  re-renders into the phone tree on its first commit without a mismatch warning. No
 *  desktop frame reaches the screen either way: the route holds a "Loading…" gate until
 *  `/api/config` answers, which resolves well after that first commit. */
export function useIsMobile(): boolean {
  return useSyncExternalStore(
    subscribe,
    () => window.matchMedia(QUERY).matches,
    () => false,
  );
}
