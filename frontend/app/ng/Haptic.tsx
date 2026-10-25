// A haptic tick for a tap on iOS. Safari plays one when a switch (`<input type="checkbox"
// switch>`, Safari 17.4 and later) is toggled by the user's own tap; a switch toggled from
// script stopped doing it in iOS 26.5. So each key carries a switch that cannot be seen, over
// all of it: the finger's tap toggles it, the tick comes as the finger lifts. The same trick as
// ios-haptics (tijnjh, MIT). Elsewhere it is a checkbox that nothing reads.

/** `switch` is not in React's or TypeScript's list of input attributes yet. */
const SWITCH = { switch: "" } as Record<string, string>;

/** Put this last in a `relative` element that is touched. Touches on it still reach the
 *  element's own handlers: they bubble up from the switch. */
export function Haptic() {
  return (
    <input
      type="checkbox"
      {...SWITCH}
      aria-hidden
      tabIndex={-1}
      className="absolute inset-0 m-0 h-full w-full opacity-0"
      style={{ clipPath: "inset(0 round 999px)", WebkitTapHighlightColor: "transparent" }}
    />
  );
}
