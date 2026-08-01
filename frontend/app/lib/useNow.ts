// The live clock, as a container-side hook.
//
// It lives here rather than inside the usage panel because "what time is it" is the same
// kind of dependency as the operator's locale: read it at a container boundary and hand the
// result down, so every leaf below draws the same thing for the same props and a story can
// pin the instant.
import { useEffect, useState } from "react";

/** Wall-clock milliseconds, refreshed every 30 seconds.
 *
 *  Null until the first effect runs, which is what keeps a prerendered pass from committing
 *  to an instant the browser will disagree with. Callers that render a clock-derived value
 *  take `number | null` and draw nothing for null. */
export function useNow(): number | null {
  const [now, setNow] = useState<number | null>(null);
  useEffect(() => {
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  return now;
}
