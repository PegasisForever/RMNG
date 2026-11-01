// What the /ng screen stories (`../NgScreenView.stories.tsx`) draw from: a picture of a clone's
// desktop in place of the video, its cursor, the pages with their own stories' data, and the
// displays the screen is shown on.
import type { Decorator } from "@storybook/react-vite";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import type { CursorShape } from "~/lib/wire/CursorShape";

import { HANDLE_H } from "../KeyboardHandle";
import { naturalKeysHeight } from "../geometry";
import ClonesStories from "../pages/ClonesView.stories";
import { ClonesView } from "../pages/ClonesView";
import NotesStories from "../pages/NotesView.stories";
import { NotesView } from "../pages/NotesView";
import SettingsStories from "../pages/SettingsView.stories";
import { SettingsView } from "../pages/SettingsView";
import TicketsStories from "../pages/TicketsView.stories";
import { TicketsView } from "../pages/TicketsView";
import UsageStories from "../pages/UsageView.stories";
import { UsageView } from "../pages/UsageView";
import type { NgTab, ServerUsage } from "../ToolRow";

/** The displays: CSS px, and the home bar's inset. */
export const PHONE = { width: 393, height: 852, bottom: 34 };
export const PHONE_SIDEWAYS = { width: 852, height: 393, bottom: 21 };
export const DESKTOP = { width: 1440, height: 900, bottom: 0 };

/** The display's frame, its size from the story's `width` and `height`, in /ng's dark greys. It
 *  holds what is fixed (a dialog) as the display would (a transform makes it their box). */
export const inDisplay: Decorator = (Story, ctx) => (
  <div
    className="ng-theme dark relative overflow-hidden bg-black text-white"
    style={{ width: ctx.args.width as number, height: ctx.args.height as number, transform: "translateZ(0)" }}
  >
    <Story />
  </div>
);

/** The server's numbers in the tool row. */
export const usage: ServerUsage = { cpu: "38%", mem: "41%", disk: "62%" };

/** The keys' state at rest. */
export const keys = {
  fn: false,
  mods: new Map(),
  caps: false,
  onKeyTouch: fn(),
  pad: false,
  dictation: "off" as const,
};

/** The keyboard's height on a display: its keys square, at most 45% of it. */
export const keysHeight = (d: { width: number; height: number; bottom: number }) =>
  Math.round(Math.min(Math.ceil(naturalKeysHeight(d.width, d.bottom) + HANDLE_H), d.height * 0.45));

/** An arrow cursor, as the daemon sends it: BGRA, premultiplied, base64. */
const ARROW = [
  "X...........",
  "XX..........",
  "XoX.........",
  "XooX........",
  "XoooX.......",
  "XooooX......",
  "XoooooX.....",
  "XooooooX....",
  "XoooooooX...",
  "XooooooooX..",
  "XoooooXXXXX.",
  "XooXooX.....",
  "XoX.XooX....",
  "XX..XooX....",
  "X....XooX...",
  ".....XooX...",
  "......XX....",
];
export const arrowCursor: CursorShape = {
  width: 12,
  height: ARROW.length,
  hotspot_x: 0,
  hotspot_y: 0,
  rgba: btoa(
    ARROW.join("")
      .split("")
      .map((c) => (c === "X" ? "\0\0\0\xff" : c === "o" ? "\xff\xff\xff\xff" : "\0\0\0\0"))
      .join(""),
  ),
} as CursorShape;

/** A clone's GNOME desktop, drawn: the top bar, a terminal and an editor. In place of the video. */
export function FakeDesktop() {
  return (
    <div className="absolute inset-0 overflow-hidden bg-[linear-gradient(135deg,#1e3a5f,#4a2c6b_60%,#a0456e)] font-sans">
      <div className="flex h-[3.2%] min-h-4 items-center justify-between bg-black px-2 text-[10px] text-white">
        <span>Activities</span>
        <span>Thu 30 Oct 14:02</span>
        <span>● ◐ ⏻</span>
      </div>
      <div className="absolute left-[4%] top-[8%] h-[52%] w-[54%] overflow-hidden rounded-lg bg-[#1e1e1e] shadow-2xl ring-1 ring-black/40">
        <div className="h-5 bg-[#2d2d2d] text-center text-[10px] leading-5 text-neutral-300">pega@pega-we-142: ~/RMNG</div>
        <pre className="p-2 font-mono text-[10px] leading-snug text-emerald-300">
          {"$ cargo test -p control-server\n   Compiling control-server v0.1.0\n    Finished test [unoptimized]\n     Running 214 tests\ntest result: ok. 214 passed\n$ "}
          <span className="bg-emerald-300 text-transparent">_</span>
        </pre>
      </div>
      <div className="absolute right-[4%] top-[30%] h-[62%] w-[50%] overflow-hidden rounded-lg bg-[#fafafa] shadow-2xl ring-1 ring-black/20">
        <div className="h-5 bg-[#ebebeb] text-center text-[10px] leading-5 text-neutral-600">FullDesk.tsx — Visual Studio Code</div>
        <pre className="p-2 font-mono text-[10px] leading-snug text-neutral-700">
          {"export function FullDesk(props) {\n  const mode = useInputMode();\n  const r = useRemote({ ... });\n  return <NgScreenView ... />;\n}"}
        </pre>
      </div>
    </div>
  );
}

/** A page the way the pages container shows it. */
function Panel({ children }: { children: ReactNode }) {
  return (
    <div role="tabpanel" className="absolute inset-0 flex select-text flex-col overflow-hidden bg-black [&_button]:select-none">
      <div className="relative min-h-0 flex-1">{children}</div>
    </div>
  );
}

/** Each tab's page, with its own story's data. */
export function pageFor(tab: NgTab): ReactNode {
  switch (tab) {
    case "usage":
      return <Panel>{<UsageView {...(UsageStories.args as Parameters<typeof UsageView>[0])} />}</Panel>;
    case "tickets":
      return <Panel>{<TicketsView {...(TicketsStories.args as Parameters<typeof TicketsView>[0])} />}</Panel>;
    case "clones":
      return <Panel>{<ClonesView {...(ClonesStories.args as Parameters<typeof ClonesView>[0])} />}</Panel>;
    case "notes":
      return <Panel>{<NotesView {...(NotesStories.args as Parameters<typeof NotesView>[0])} />}</Panel>;
    case "settings":
      return <Panel>{<SettingsView {...(SettingsStories.args as Parameters<typeof SettingsView>[0])} />}</Panel>;
  }
}
