// `/ng/app`: the selected clone's screen, on a phone (the installed app: Add to Home Screen;
// `public/manifest.webmanifest` opens `/ng`) and on a desktop, with touch or with a mouse and a
// keyboard (`~/ng/FullDesk.tsx`). `/ng` is the shell that holds it in a frame
// (`public/ng-shell.html`), or sends a browser with no fullscreen here. `?monitor=N` shows
// monitor N of this browser's layout (a desktop has one window per monitor); `?control-panel` shows
// only the control panel, in a window of its own (`~/ng/ControlPanelWindow.tsx`). The update rules are in
// `~/ng/pwa.ts`.
import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";

import { shellWindow } from "~/lib/host";
import { FullDesk } from "~/ng/FullDesk";
import { ControlPanelWindow } from "~/ng/ControlPanelWindow";
import { attachPwa } from "~/ng/pwa";
import { isTyping } from "~/ng/systemKeyboard";

export function meta() {
  return [{ title: "RMNG" }];
}

/** The page runs under the clock bar and the home bar, and cannot be zoomed. */
const VIEWPORT =
  "width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no, viewport-fit=cover";

type Page = { size: [number, number]; top: number; bottom: number };

function measure(probe: HTMLElement): Page {
  // A frame has no home bar inset of its own: the shell's window has it
  const shell = shellWindow();
  return {
    size: [window.innerWidth, window.innerHeight],
    top: Math.round(shell ? (shell.rmngShell.insetTop?.() ?? 0) : parseFloat(getComputedStyle(probe).paddingTop) || 0),
    bottom: Math.round(shell ? shell.rmngShell.insetBottom() : parseFloat(getComputedStyle(probe).paddingBottom) || 0),
  };
}

export default function Ng() {
  const [params, setParams] = useSearchParams();
  const monitor = Number(params.get("monitor") ?? 0) || 0;
  const controlPanelOnly = params.has("control-panel");
  const setMonitor = (next: number) => {
    // The browser window's address too, so a reload of the shell shows the same monitor
    shellWindow()?.history.replaceState(null, "", `/ng?monitor=${next}`);
    setParams(
      (p) => {
        p.set("monitor", String(next));
        return p;
      },
      { replace: true },
    );
  };
  const probe = useRef<HTMLDivElement>(null);
  const [page, setPage] = useState<Page | null>(null);
  // Null until the effect has looked: the page has no `window` while it is built
  const [secure, setSecure] = useState<boolean | null>(null);

  useEffect(() => {
    setSecure(window.isSecureContext);
    const meta = document.querySelector<HTMLMetaElement>('meta[name="viewport"]');
    const before = meta?.content;
    if (meta) meta.content = VIEWPORT;
    // Black under the page too: iOS shows the strip below the page size
    const { documentElement: html, body } = document;
    html.style.background = body.style.background = "#000";
    // /ng's greys (app.css); light or dark as the OS says, as the rest of the app
    html.classList.add("ng-theme");
    body.style.overflow = "hidden";
    // iOS 26+ gives an installed app a page shorter than the screen while the document is
    // shorter than the screen, and paints nothing below that edge (WebKit bug 301108; the fix
    // from frappe-ui#1284). A document as tall as the screen gets the whole screen.
    html.style.minHeight = "min(100lvh, calc(100% + env(safe-area-inset-top)))";

    // The phone's keyboard can make the page shorter: while it is up the page stays as it was
    const update = () => probe.current && !isTyping() && setPage(measure(probe.current));
    update();
    // iOS can settle the viewport of a first open after the page loaded, with no resize event
    const settle = [100, 500, 1500].map((ms) => setTimeout(update, ms));
    window.addEventListener("resize", update);
    window.addEventListener("orientationchange", update);
    window.visualViewport?.addEventListener("resize", update);
    // and is measured again once the keyboard went down
    const onFocusOut = () => setTimeout(update, 600);
    document.addEventListener("focusout", onFocusOut);
    const detach = attachPwa();
    return () => {
      detach();
      settle.forEach(clearTimeout);
      window.removeEventListener("resize", update);
      window.removeEventListener("orientationchange", update);
      window.visualViewport?.removeEventListener("resize", update);
      document.removeEventListener("focusout", onFocusOut);
      if (meta && before !== undefined) meta.content = before;
      html.style.background = body.style.background = "";
      html.classList.remove("ng-theme");
      html.style.minHeight = "";
      body.style.overflow = "";
    };
  }, []);

  const message = secure === false ? "This page is not HTTPS: open it at the server's HTTPS address." : null;

  return (
    <div className="fixed inset-0 touch-none select-none overflow-hidden bg-black text-white">
      <div ref={probe} className="pointer-events-none absolute inset-0 pb-[env(safe-area-inset-bottom)] pt-[env(safe-area-inset-top)]" />
      {message && page ? (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 p-8 text-center text-slate-500">
          <div>{message}</div>
          <div className="font-mono text-sm">
            page {page.size[0]} × {page.size[1]}
          </div>
        </div>
      ) : null}
      {secure && page && controlPanelOnly ? <ControlPanelWindow width={page.size[0]} height={page.size[1]} /> : null}
      {secure && page && !controlPanelOnly ? (
        <FullDesk
          width={page.size[0]}
          height={page.size[1]}
          top={page.top}
          bottom={page.bottom}
          monitor={monitor}
          onMonitor={setMonitor}
        />
      ) : null}
    </div>
  );
}
