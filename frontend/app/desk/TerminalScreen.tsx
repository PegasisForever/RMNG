// A headless clone's terminal, in monitor 0's window in place of the clone's screen: the
// tab bar (`./TermBar.tsx`) and one xterm.js terminal per tmux session (`~/viewer/terminal.tsx`).
//
// - Mouse and keyboard mode: the terminal has the page's keys, mouse and selection. The control panel's chord is the page's
//   (`./keys.ts` runs it).
// - Touch mode: the on-screen keyboard types into it (`./remote.ts`, `./termKeys.ts`); the terminal never
//   brings up the system keyboard. Its area is a trackpad, as the keys are: they move a pointer
//   drawn here (`cursor`), and its clicks, drags and scrolls go to the terminal as a mouse's.
//
// The drawing is `./TerminalScreenView.tsx`'s.
import type { PointerEvent } from "react";

import type { FromBrowser } from "~/lib/wire/FromBrowser";
import type { TermData } from "~/lib/wire/TermData";
import { type TermApi, useTermTabs } from "~/viewer/terminal";

import type { TouchKind } from "./KeyboardView";
import { isControlPanelChord } from "./keys";
import { TerminalScreenView } from "./TerminalScreenView";

export function TerminalScreen({
  sessions,
  send,
  sink,
  api,
  touch,
  top,
  cursor,
  onPad,
}: {
  sessions: string[];
  send: (msg: FromBrowser) => void;
  /** The page puts each `term_data` it gets into this. */
  sink: { current: ((d: TermData) => void) | null };
  /** Set to the shown tab's terminal. */
  api: { current: TermApi | null };
  /** Touch mode: the trackpad, the drawn pointer, no system keyboard. */
  touch: boolean;
  /** The clock bar's safe-area inset, CSS px. */
  top: number;
  /** The drawn pointer, CSS px in the terminal; null: none yet. */
  cursor: { x: number; y: number } | null;
  /** A finger on the terminal's area (touch mode). */
  onPad: (kind: TouchKind, e: PointerEvent<HTMLDivElement>) => void;
}) {
  const { box, active, setActive } = useTermTabs({
    sessions,
    send,
    sink,
    api,
    focus: !touch,
    // The chord is the page's: xterm.js does nothing with it, and the window's keys run it
    ownKey: (e) => isControlPanelChord(e),
  });
  return (
    <TerminalScreenView
      sessions={sessions}
      active={active}
      onPick={setActive}
      onNew={() => send({ t: "term_new_session" })}
      top={top}
      touch={touch}
      cursor={cursor}
      onPad={onPad}
    >
      <div ref={box} className="absolute inset-0" />
    </TerminalScreenView>
  );
}
