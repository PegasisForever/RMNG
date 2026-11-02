// `/app`: the selected clone's screen, on a phone (the installed app: Add to Home Screen;
// `public/manifest.webmanifest` opens `/`) and on a desktop, with touch or with a mouse and a
// keyboard (`~/desk/FullDesk.tsx`). `/` is the shell that holds it in a frame
// (`public/shell.html`), or sends a browser with no fullscreen here. `?monitor=N` shows
// monitor N of this browser's layout (a desktop has one window per monitor); `?control-panel` shows
// only the control panel, in a window of its own (`~/desk/ControlPanelWindow.tsx`). The update rules are in
// `~/desk/pwa.ts`. A server whose first-run setup is not done shows the setup wizard in place of
// all of it.
import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";

import { ConfirmLayer } from "~/components/ConfirmLayer";
import { SetupWizardContainer } from "~/components/SetupWizardContainer";
import { TooltipLayer } from "~/components/TooltipLayer";
import { getConfig } from "~/lib/api";
import { shellWindow } from "~/lib/host";
import type { AppConfigRedacted } from "~/lib/wire/AppConfigRedacted";
import { FullDesk } from "~/desk/FullDesk";
import { ControlPanelWindow } from "~/desk/ControlPanelWindow";
import type { Insets } from "~/desk/geometry";
import { attachPwa } from "~/desk/pwa";
import { isTyping } from "~/desk/systemKeyboard";

export function meta() {
  return [{ title: "RMNG" }];
}

type Page = { size: [number, number]; safe: Insets };

function measure(probe: HTMLElement): Page {
  // A frame has no insets of its own: the shell's window has them
  const shell = shellWindow()?.rmngShell;
  const s = getComputedStyle(probe);
  const px = (v: string) => parseFloat(v) || 0;
  return {
    size: [window.innerWidth, window.innerHeight],
    safe: {
      top: Math.round(shell ? shell.insetTop() : px(s.paddingTop)),
      right: Math.round(shell ? shell.insetRight() : px(s.paddingRight)),
      bottom: Math.round(shell ? shell.insetBottom() : px(s.paddingBottom)),
      left: Math.round(shell ? shell.insetLeft() : px(s.paddingLeft)),
    },
  };
}

/** The insets as CSS variables on the page, for what is drawn outside the screen's view (a
 *  dialog on the body, the monitor's number): `--safe-top`… in a frame, where
 *  `env(safe-area-inset-*)` is 0. */
function shareInsets(safe: Insets | null) {
  const style = document.documentElement.style;
  for (const side of ["top", "right", "bottom", "left"] as const) {
    if (safe) style.setProperty(`--safe-${side}`, `${safe[side]}px`);
    else style.removeProperty(`--safe-${side}`);
  }
}

export default function App() {
  const [params, setParams] = useSearchParams();
  const monitor = Number(params.get("monitor") ?? 0) || 0;
  const controlPanelOnly = params.has("control-panel");
  const setMonitor = (next: number) => {
    setParams(
      (p) => {
        p.set("monitor", String(next));
        return p;
      },
      { replace: true },
    );
  };
  // The browser window's address is this frame's (`?monitor=`, the open clone and ticket), so a
  // reload of the shell, or its address copied, shows the same
  const search = params.toString();
  useEffect(() => {
    shellWindow()?.history.replaceState(null, "", search ? `/?${search}` : "/");
  }, [search]);
  const probe = useRef<HTMLDivElement>(null);
  const [page, setPage] = useState<Page | null>(null);
  // Null until the effect has looked: the page has no `window` while it is built
  const [secure, setSecure] = useState<boolean | null>(null);
  /** The first-run setup: unknown (null) until the config is read, then whether it is done.
   *  A config that cannot be read counts as done: the app then shows its own errors. */
  const [setup, setSetup] = useState<{ done: true } | { done: false; config: AppConfigRedacted } | null>(null);
  const readSetup = () => {
    getConfig()
      .then((c) => setSetup(c.setupComplete ? { done: true } : { done: false, config: c }))
      .catch(() => setSetup({ done: true }));
  };
  useEffect(readSetup, []);

  useEffect(() => {
    setSecure(window.isSecureContext);

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
      shareInsets(null);
    };
  }, []);

  const safeKey = page ? JSON.stringify(page.safe) : "";
  useEffect(() => {
    if (safeKey) shareInsets(JSON.parse(safeKey) as Insets);
  }, [safeKey]);

  if (setup && !setup.done) {
    // The wizard, on its own page: it scrolls, and its text can be selected
    return (
      <div className="fixed inset-0 select-text overflow-auto">
        <SetupWizardContainer initialConfig={setup.config} onDone={readSetup} />
        <TooltipLayer />
      </div>
    );
  }

  const message = secure === false ? "This page is not HTTPS: open it at the server's HTTPS address." : null;

  return (
    <div className="fixed inset-0 touch-none select-none overflow-hidden bg-black text-white">
      <div ref={probe} className="pointer-events-none absolute inset-0 pb-[env(safe-area-inset-bottom)] pl-[env(safe-area-inset-left)] pr-[env(safe-area-inset-right)] pt-[env(safe-area-inset-top)]" />
      {message && page ? (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 p-8 text-center text-slate-500">
          <div>{message}</div>
          <div className="font-mono text-sm">
            page {page.size[0]} × {page.size[1]}
          </div>
        </div>
      ) : null}
      {secure && page && setup && controlPanelOnly ? <ControlPanelWindow width={page.size[0]} /> : null}
      {secure && page && setup && !controlPanelOnly ? (
        <FullDesk
          width={page.size[0]}
          height={page.size[1]}
          safe={page.safe}
          monitor={monitor}
          onMonitor={setMonitor}
        />
      ) : null}
      {/* The tooltips and the questions of every part of the page */}
      <TooltipLayer />
      <ConfirmLayer />
    </div>
  );
}
