import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { LayoutEditorView } from "~/components/LayoutEditorView";
import { DEFAULT_DESKTOP_LAYOUT } from "~/lib/clientLayout";

import {
  DESKTOP,
  FakeDesktop,
  FakeTerminal,
  PHONE,
  PHONE_SIDEWAYS,
  arrowCursor,
  inDisplay,
  keys,
  keysHeight,
  pageFor,
  usage,
} from "./__fixtures__/screen";
import { NgScreenView, type NgScreenViewProps } from "./NgScreenView";
import type { NgTab } from "./ToolRow";

/** The cursor at 1.5 CSS px per clone px, as on a phone at 2× with a 3× screen. */
const cursorScale = () => 1.5 * (typeof window === "undefined" ? 1 : window.devicePixelRatio || 1);

const buttons = (over: Partial<NonNullable<NgScreenViewProps["buttons"]>> = {}): NgScreenViewProps["buttons"] => ({
  pagesOpen: false,
  keyboardOpen: false,
  keyboardDocked: false,
  fit: { fits: true, onFit: fn() },
  badge: 0,
  onPages: fn(),
  onKeyboard: fn(),
  onKeyboardDock: fn(),
  onLayout: fn(),
  fullscreen: { on: false, toggle: fn() },
  ...over,
});

const pages = (over: Partial<NgScreenViewProps["pages"]> = {}): NgScreenViewProps["pages"] => {
  const tab = over.tab ?? "clones";
  return { open: false, tab, onTab: fn(), usage, lift: 0, content: pageFor(tab), ...over };
};

const keyboard = (d: typeof PHONE, docked = false): NgScreenViewProps["keyboard"] => ({
  docked,
  height: keysHeight(d),
  keys,
  onHeight: fn(),
  onHeightDone: fn(),
});

const notes = { dictation: "off" as const, heard: "", note: null };

/** A phone, upright: the clone's screen fills the display. */
const meta = {
  title: "ng/Pages/NgScreenView",
  component: NgScreenView,
  decorators: [inDisplay],
  parameters: { layout: "centered" },
  args: {
    ...PHONE,
    mode: "touch",
    screen: { x: 0, y: 0, width: PHONE.width, height: PHONE.height },
    picture: <FakeDesktop />,
    cursor: { shape: arrowCursor, x: 120, y: 300, scale: cursorScale() },
    onPad: fn(),
    onTouchScreen: fn(),
    keyboard: null,
    notes,
    pages: pages(),
    buttons: buttons(),
    identify: null,
  },
} satisfies Meta<typeof NgScreenView>;

export default meta;
type Story = StoryObj<typeof meta>;

// ---- touch mode, on a phone ----

/** The screen and the three buttons (Pages, Keyboard, Fullscreen, Fit). */
export const Phone: Story = {};

/** The keyboard floats over the bottom of the screen; the screen moved up to keep the cursor
 *  above it. */
export const PhoneKeyboard: Story = {
  args: {
    keyboard: keyboard(PHONE),
    screen: { x: 0, y: -120, width: PHONE.width, height: PHONE.height },
    buttons: buttons({ keyboardOpen: true }),
  },
};

/** The keyboard docked: the screen fits in the area above it, a touch screen. */
export const PhoneKeyboardDocked: Story = {
  args: {
    keyboard: keyboard(PHONE, true),
    screen: { x: 59, y: 0, width: PHONE.width - 118, height: PHONE.height - keysHeight(PHONE) },
    buttons: buttons({ keyboardOpen: true, keyboardDocked: true }),
  },
};

/** The pages over the screen, on the Clones tab; two clones stopped working unseen. */
export const PhonePages: Story = {
  args: {
    pages: pages({ open: true, badges: { clones: 2 } }),
    buttons: buttons({ pagesOpen: true, badge: 2 }),
  },
};

/** Dictation hears words; the monitor is not the display's size, so Fit is live. */
export const PhoneDictation: Story = {
  args: {
    notes: { ...notes, dictation: "listening", heard: "run the tests again please" },
    keyboard: { ...keyboard(PHONE)!, keys: { ...keys, dictation: "listening" } },
    screen: { x: 0, y: -120, width: PHONE.width, height: PHONE.height },
    buttons: buttons({ keyboardOpen: true, fit: { fits: false, onFit: fn() } }),
  },
};

/** The clones are in use on the desktop. */
export const PhoneInUseElsewhere: Story = {
  args: { notes: { ...notes, note: "In use on the desktop. Touch to use it here." } },
};

/** No connection yet: no picture. */
export const PhoneConnecting: Story = {
  args: { picture: null, cursor: null, notes: { ...notes, note: "Connecting…" } },
};

/** A phone on its side, with the pages on the Tickets tab. */
export const PhoneSideways: Story = {
  args: {
    ...PHONE_SIDEWAYS,
    screen: { x: 0, y: 0, width: PHONE_SIDEWAYS.width, height: PHONE_SIDEWAYS.height },
    pages: pages({ open: true, tab: "tickets" }),
    buttons: buttons({ pagesOpen: true }),
  },
};

/** A long press on Fit: the Edit layout dialog, with the phone's one monitor (no windows to
 *  open, none to add). */
export const PhoneLayoutDialog: Story = {
  args: {
    dialog: (
      <LayoutEditorView
        layout={{
          ...DEFAULT_DESKTOP_LAYOUT,
          monitors: [{ width: 590, height: 1278, x: 0, y: 0, primary: true }],
          scales: [2],
        }}
        onChange={fn()}
        onMatchWindow={fn()}
        matching={null}
        note={null}
        onClose={fn()}
        glass
      />
    ),
  },
};

/** A headless clone: its terminal in place of the screen, the keyboard always under it, and
 *  the pointer the keys move drawn over it. The buttons are in the terminal's tab bar (no Fit). */
export const PhoneTerminal: Story = {
  args: {
    picture: null,
    cursor: null,
    terminal: <FakeTerminal cursor={{ x: 180, y: 120 }} />,
    keyboard: keyboard(PHONE, true),
    buttons: buttons({ keyboardOpen: true, fit: undefined }),
  },
};

/** A headless clone with the keyboard hidden: the terminal down to the home bar. */
export const PhoneTerminalNoKeyboard: Story = {
  args: { picture: null, cursor: null, terminal: <FakeTerminal />, buttons: buttons({ fit: undefined }) },
};

// ---- mouse and keyboard mode, on a desktop ----

const desktop = {
  ...DESKTOP,
  mode: "mouse" as const,
  screen: { x: 0, y: 0, width: DESKTOP.width, height: DESKTOP.height },
  cursor: null,
  buttons: buttons({ keyboard: false, fit: undefined, away: true }),
};

/** Only the clone's screen: the buttons slid up out of the window. */
export const Desktop: Story = { args: { ...desktop } };

/** The mode just started, or the mouse is at the window's right edge by the buttons' row: the
 *  buttons are in (no keyboard button). */
export const DesktopButtonsIn: Story = {
  args: { ...desktop, buttons: buttons({ keyboard: false, fit: undefined }) },
};

/** First open of the site with a layout of three monitors: this window shows monitor 0, and
 *  the button at the left opens the other two. */
export const DesktopMonitorsToOpen: Story = {
  args: { ...desktop, buttons: buttons({ keyboard: false, fit: undefined, monitors: { missing: [1, 2], onOpen: fn() } }) },
};

/** The chord showed the pages, at the right, with the buttons above them (no keyboard button)
 *  and the button that moves them to a window of their own. */
export const DesktopPages: Story = {
  args: {
    ...desktop,
    pages: pages({ open: true, onPopOut: fn() }),
    buttons: buttons({ pagesOpen: true, keyboard: false, fit: undefined }),
  },
};

/** A headless clone: its terminal, with the page's keys and mouse (xterm.js's own). */
export const DesktopTerminal: Story = {
  args: { ...desktop, picture: null, terminal: <FakeTerminal touch={false} />, buttons: buttons({ keyboard: false, fit: undefined }) },
};

/** A headless clone in monitor 1's window: its terminal is in monitor 0's. */
export const DesktopTerminalOtherMonitor: Story = {
  args: {
    ...desktop,
    picture: null,
    terminal: <div className="absolute inset-0 bg-black" />,
    notes: { ...notes, note: "This clone has no desktop. Its terminal is in the monitor 0 window." },
  },
};

/** The Edit layout dialog is open in another window: this one shows its monitor's number. */
export const DesktopIdentify: Story = {
  args: { ...desktop, identify: 1 },
};

/** A right click on Fullscreen: the Edit layout dialog over the pages, and this window's
 *  monitor's number in its corner. */
export const DesktopLayoutDialog: Story = {
  args: {
    ...desktop,
    pages: pages({ open: true, onPopOut: fn() }),
    buttons: buttons({ pagesOpen: true, keyboard: false, fit: undefined }),
    dialog: (
      <LayoutEditorView
        layout={DEFAULT_DESKTOP_LAYOUT}
        onChange={fn()}
        onMatchWindow={fn()}
        onOpenWindow={fn()}
        here={0}
        matching={null}
        note={null}
        onClose={fn()}
        glass
      />
    ),
    identify: 0,
  },
};

// ---- to play with ----

/** The buttons work: Pages and the tabs, Keyboard (tap: floating; long press: docked), Fit. */
export const PhoneInteractive: Story = {
  render: function Render(args) {
    const [open, setOpen] = useState(false);
    const [tab, setTab] = useState<NgTab>("clones");
    const [shown, setShown] = useState(false);
    const [docked, setDocked] = useState(false);
    const [fits, setFits] = useState(false);
    const kb = shown || docked ? keyboard(PHONE, docked) : null;
    const keysH = kb?.height ?? 0;
    return (
      <NgScreenView
        {...args}
        screen={
          docked
            ? { x: 59, y: 0, width: PHONE.width - 118, height: PHONE.height - keysH }
            : { x: 0, y: shown ? -120 : 0, width: PHONE.width, height: PHONE.height }
        }
        cursor={{ shape: arrowCursor, x: 120, y: 300, scale: cursorScale() }}
        keyboard={kb}
        pages={pages({ open, tab, onTab: setTab, content: pageFor(tab) })}
        buttons={buttons({
          pagesOpen: open,
          keyboardOpen: !!kb,
          keyboardDocked: docked,
          fit: { fits, onFit: () => setFits(true) },
          onPages: () => setOpen((o) => !o),
          onKeyboard: () => !docked && setShown((s) => !s),
          onKeyboardDock: () => {
            setDocked((d) => !d);
            setShown(false);
          },
        })}
      />
    );
  },
};

/** The mouse at the display's right edge, as high as the buttons, brings them in; away from
 *  them it sends them away.
 *  The button at the bottom left stands for Super+Space: it shows or hides the pages. */
export const DesktopInteractive: Story = {
  args: { ...desktop },
  render: function Render(args) {
    const [open, setOpen] = useState(false);
    const [tab, setTab] = useState<NgTab>("clones");
    const [peek, setPeek] = useState(false);
    return (
      <>
        <div
          onPointerMove={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            const [x, y] = [e.clientX - r.left, e.clientY - r.top];
            if (x >= r.width - 2 && y <= 60) setPeek(true);
            else if (y > 84 || x < r.width - 260) setPeek(false);
          }}
        >
          <NgScreenView
            {...args}
            pages={pages({ open, tab, onTab: setTab, content: pageFor(tab), onPopOut: fn() })}
            buttons={buttons({
              pagesOpen: open,
              keyboard: false,
              fit: undefined,
              onPages: () => setOpen((o) => !o),
              away: !open && !peek,
            })}
          />
        </div>
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          className="absolute bottom-3 left-3 z-50 rounded bg-white/90 px-2 py-1 text-xs text-black"
        >
          Super Space
        </button>
      </>
    );
  },
};
