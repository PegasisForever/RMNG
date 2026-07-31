import { useEffect, useState } from "react";

/** Whether dark is in force, by the same rule the `dark:` CSS variant uses (see app.css):
 *  the OS asks for it, or `<html>` carries `.dark`. Storybook's theme toolbar sets that
 *  class, which is the only way to preview dark on a light-OS machine. */
function darkNow(): boolean {
  return (
    window.matchMedia("(prefers-color-scheme: dark)").matches ||
    document.documentElement.classList.contains("dark")
  );
}

/** Live light/dark, for third-party widgets that theme through a JS prop instead of CSS.
 *  BlockNote is the one that matters: its text colour comes from that prop, so a widget
 *  left on light inside a dark pane paints near-black text on a near-black surface.
 *
 *  Reading the class as well as the media query is what makes the Storybook toggle honest.
 *  Keying off the media query alone left the notes editor light while everything around it
 *  went dark, which looks like a styling bug in the editor rather than a preview that only
 *  half applies.
 *
 *  Answers synchronously on the first client render (guarded for SSR) so a freshly mounted
 *  widget paints the right theme immediately — otherwise a remount, and switching clones
 *  re-keys the notes editor, flashes light before the effect corrects it. Falls back to
 *  `"light"` when `window` is absent. */
export function useColorScheme(): "light" | "dark" {
  const [scheme, setScheme] = useState<"light" | "dark">(() =>
    typeof window !== "undefined" && darkNow() ? "dark" : "light",
  );
  useEffect(() => {
    const apply = () => setScheme(darkNow() ? "dark" : "light");
    apply();

    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", apply);
    // The class hook changes with no event of its own, so watch the attribute itself.
    const observer = new MutationObserver(apply);
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    });

    return () => {
      mq.removeEventListener("change", apply);
      observer.disconnect();
    };
  }, []);
  return scheme;
}
