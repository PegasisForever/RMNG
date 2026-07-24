import { useEffect, useState } from "react";

/** Live `prefers-color-scheme` — `"dark"` when the OS is in dark mode, else `"light"`.
 *  Tailwind's `dark:` variant handles our own utility classes automatically via the
 *  same media query; this hook is for third-party widgets (e.g. BlockNote) that theme
 *  through a JS prop instead of CSS, so they can follow the system setting too.
 *  Reads the OS preference synchronously on the first client render (guarded for SSR)
 *  so a freshly-mounted widget paints the right theme immediately — otherwise a
 *  remount (e.g. switching clones re-keys the notes editor) flashes light before the
 *  effect corrects it. Falls back to `"light"` when `window` is absent (SSR). */
export function useColorScheme(): "light" | "dark" {
  const [scheme, setScheme] = useState<"light" | "dark">(() =>
    typeof window !== "undefined" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches
      ? "dark"
      : "light",
  );
  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => setScheme(mq.matches ? "dark" : "light");
    apply();
    mq.addEventListener("change", apply);
    return () => mq.removeEventListener("change", apply);
  }, []);
  return scheme;
}
