// What the screen stories (`../ScreenView.stories.tsx`) draw from: a picture of a clone's
// desktop in place of the video, its cursor, the control panel with its tabs' own stories' data, and the
// displays the screen is shown on.
import type { Decorator } from "@storybook/react-vite";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import type { Operation } from "~/lib/types";
import type { CursorShape } from "~/lib/wire/CursorShape";

export { FakeDesktop } from "./desktop";
import { HANDLE_H } from "../KeyboardHandle";
import { type Insets, NO_INSETS, naturalKeysHeight } from "../geometry";
import { TerminalScreenView } from "../TerminalScreenView";
import ClonesStories from "../control/ClonesView.stories";
import { ClonesView } from "../control/ClonesView";
import NotesStories from "../control/NotesView.stories";
import { NotesView } from "../control/NotesView";
import SettingsStories from "../control/SettingsView.stories";
import { SettingsView } from "../control/SettingsView";
import TicketsStories from "../control/TicketsView.stories";
import { TicketsView } from "../control/TicketsView";
import { makeUsage } from "~/components/__fixtures__/accounts";
import UsageStories from "../control/UsageView.stories";
import { UsageView } from "../control/UsageView";
import { resolveColumns, withDefaults } from "~/lib/board";

import { BoardDnd } from "../control/BoardDnd";
import { NarrowLayout } from "../control/NarrowLayout";
import { SidePanels, sideWidthPx, WIDE_MIN, WideLayout } from "../control/WideLayout";
import { SIDE_DEFAULT } from "~/lib/sidePanelWidth";
import { type PanelTab, ROW_STATS_MIN, type ServerUsage } from "../ToolRow";

/** The displays: CSS px, and their unsafe edges (an iPhone's: the clock bar and the camera
 *  upright, the camera at a side and the home bar sideways). */
export const PHONE = { width: 393, height: 852, safe: { top: 59, right: 0, bottom: 34, left: 0 } };
export const PHONE_SIDEWAYS = { width: 852, height: 393, safe: { top: 0, right: 59, bottom: 21, left: 59 } };
export const DESKTOP = { width: 1440, height: 900, safe: NO_INSETS };
/** The width of a display's safe area. */
export const safeWidth = (d: { width: number; safe: Insets }) => d.width - d.safe.left - d.safe.right;

/** The display's frame, its size from the story's `width` and `height`, in the app's greys, light or
 *  dark by the Theme in the toolbar (the app: by the OS). It holds what is fixed (a dialog) as
 *  the display would (a transform makes it their box). */
export const inDisplay: Decorator = (Story, ctx) => (
  <div
    className="relative overflow-hidden bg-black text-neutral-900 dark:text-white"
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
export const keysHeight = (d: { width: number; height: number; safe: Insets }) =>
  Math.round(Math.min(Math.ceil(naturalKeysHeight(safeWidth(d), d.safe.bottom) + HANDLE_H), d.height * 0.45));

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


/** A headless clone's terminal, drawn: its tab bar and a shell in tmux, in place of xterm.js.
 *  `cursor`: the drawn pointer (touch mode). */
export function FakeTerminal({ touch = true, cursor = null }: { touch?: boolean; cursor?: { x: number; y: number } | null }) {
  const g = "text-[#57e389]";
  const b = "text-[#51a1ff]";
  return (
    <TerminalScreenView sessions={["main", "build"]} active="main" onPick={fn()} onNew={fn()} top={0} touch={touch} cursor={cursor} onPad={fn()}>
      <pre className="absolute inset-0 overflow-hidden font-mono text-[14px] leading-[1.1] text-white">
        <span className={g}>rmng@pega-we-151</span>:<span className={b}>~/RMNG</span>$ cargo test -p wire{"\n"}
        {"   Compiling wire v0.1.0\n    Finished test [unoptimized] in 4.2s\n     Running 58 tests\n"}
        {"test result: "}<span className={g}>ok</span>{". 58 passed; 0 failed\n"}
        <span className={g}>rmng@pega-we-151</span>:<span className={b}>~/RMNG</span>$ git status --short{"\n"}
        {" M frontend/app/desk/TerminalScreen.tsx\n"}
        <span className={g}>rmng@pega-we-151</span>:<span className={b}>~/RMNG</span>$ <span className="bg-white text-transparent">_</span>
      </pre>
    </TerminalScreenView>
  );
}

/** A page the way the control panel container shows it (narrow: a tab's page). */
function Panel({ children }: { children: ReactNode }) {
  return (
    <div role="tabpanel" className="absolute inset-0 flex select-text flex-col overflow-hidden [&_button]:select-none">
      <div className="relative min-h-0 flex-1">{children}</div>
    </div>
  );
}

const usageArgs = UsageStories.args as Parameters<typeof UsageView>[0];

/** The rail's accounts: the Usage story's, and a pool of five more, so the rail is longer than
 *  the display and scrolls. */
const railArgs: Parameters<typeof UsageView>[0] = (() => {
  const at = (hours: number) => new Date((usageArgs.now ?? 0) + hours * 3_600_000).toISOString();
  const more = ["kim", "lee", "max", "noa", "ray"].map((name, i) =>
    makeUsage({
      email: `${name}@example.com`,
      fiveHour: { pct: 15 + i * 17, resetsAt: at(1 + i) },
      sevenDay: { pct: 30 + i * 12, resetsAt: at(30 + i * 20) },
    }),
  );
  return {
    ...usageArgs,
    accounts: [...usageArgs.accounts, ...more],
    groups: [...usageArgs.groups, { name: "night", accounts: more.map((a) => ({ email: a.email })) }],
  };
})();
const ticketsArgs = TicketsStories.args as Parameters<typeof TicketsView>[0];
const clonesArgs = ClonesStories.args as Parameters<typeof ClonesView>[0];
const notesArgs = NotesStories.args as Parameters<typeof NotesView>[0];
const settingsArgs = SettingsStories.args as Parameters<typeof SettingsView>[0];

/** Each tab's page, with its own story's data. `server`: the server's numbers at the top of
 *  Usage (the tool row has no room for them). */
export function pageFor(tab: PanelTab, server = false): ReactNode {
  return withDnd(tab, page(tab, server));
}

/** The clone columns of the Clones page's story, for the drags. */
const lanes = () =>
  resolveColumns(withDefaults(clonesArgs.columns), clonesArgs.clones).map((c) => ({ id: c.id, cloneIds: c.cloneIds }));

/** What drags on a page, as the container has it (a drop is only logged). */
function withDnd(tab: PanelTab, node: ReactNode): ReactNode {
  if (tab === "clones") return <BoardDnd lanes={lanes()} onMoveCard={fn()}>{node}</BoardDnd>;
  if (tab === "tickets")
    return (
      <BoardDnd tickets={ticketsArgs.tickets.map((t) => t.id)} onReorderTickets={fn()}>
        {node}
      </BoardDnd>
    );
  return node;
}

function page(tab: PanelTab, server: boolean): ReactNode {
  switch (tab) {
    case "usage":
      return <Panel>{<UsageView {...usageArgs} server={server ? usage : undefined} />}</Panel>;
    case "tickets":
      return <Panel>{<TicketsView {...ticketsArgs} />}</Panel>;
    case "clones":
      return <Panel>{<ClonesView {...clonesArgs} />}</Panel>;
    case "notes":
      return <Panel>{<NotesView {...notesArgs} pane="notes" />}</Panel>;
    case "assistant":
      return <Panel>{<NotesView {...notesArgs} pane="chat" />}</Panel>;
    case "settings":
      return <Panel>{<SettingsView {...settingsArgs} />}</Panel>;
  }
}

/** The side panels, wide: the clone's notes over its assistant, with the Notes page's data. */
export function sideFor(): ReactNode {
  return (
    <SidePanels
      notes={<NotesView {...notesArgs} only="notes" />}
      chat={<NotesView {...notesArgs} only="chat" />}
      width={sideWidthPx(DESKTOP.width, SIDE_DEFAULT)}
      onWidth={fn()}
      onWidthDone={fn()}
      onWidthReset={fn()}
    />
  );
}

/** The control panel as the container lays it out `width` CSS px wide: the tool row over a tab's page,
 *  or (wide) the columns. */
export function controlPanelFor({
  width,
  glass = true,
  tab,
  onTab = fn(),
  badges,
  onPopOut,
  operations,
}: {
  width: number;
  /** Over the clone's screen (wide: each column glass), not in the control panel's own window. */
  glass?: boolean;
  tab: PanelTab;
  onTab?: (tab: PanelTab) => void;
  badges?: Partial<Record<PanelTab, number>>;
  onPopOut?: () => void;
  /** The server's jobs, at the top of the wide layout's rail; absent: the Usage story's. */
  operations?: Operation[];
}): ReactNode {
  if (width >= WIDE_MIN) {
    return (
      <div className={`flex h-full flex-col ${glass ? "" : "p-2"}`}>
        <WideLayout
          width={width}
          glass={glass}
          usage={usage}
          settingsOpen={tab === "settings"}
          onSettings={() => onTab(tab === "settings" ? "clones" : "settings")}
          onPopOut={onPopOut}
          onLayout={glass ? fn() : undefined}
          rail={<UsageView {...railArgs} operations={operations ?? railArgs.operations} rail />}
          board={
            <BoardDnd
              lanes={lanes()}
              tickets={ticketsArgs.tickets.map((t) => t.id)}
              onMoveCard={fn()}
              onReorderTickets={fn()}
              onTicketToColumn={fn()}
            >
              <TicketsView {...ticketsArgs} column />
              <ClonesView {...clonesArgs} columnsLayout onRenameColumn={fn()} />
            </BoardDnd>
          }
          side={{
            notes: <NotesView {...notesArgs} only="notes" />,
            chat: <NotesView {...notesArgs} only="chat" />,
            widthPct: SIDE_DEFAULT,
          }}
          onSideWidthCommit={fn()}
          settings={<SettingsView {...settingsArgs} />}
        />
      </div>
    );
  }
  return (
    <NarrowLayout row={{ width: width - 12, usage, tab, onTab, badges, onPopOut }}>
      {pageFor(tab, width - 12 < ROW_STATS_MIN)}
    </NarrowLayout>
  );
}
