import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { LayoutEditorView } from "~/components/LayoutEditorView";
import { DEFAULT_DESKTOP_LAYOUT } from "~/lib/clientLayout";

import {
  DESKTOP,
  FakeDesktop,
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
  fits: true,
  badge: 0,
  onPages: fn(),
  onKeyboard: fn(),
  onKeyboardDock: fn(),
  onFit: fn(),
  onFitSettings: fn(),
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

const notes = { dictation: "off" as const, heard: "", note: null, hint: null };

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
    buttons: buttons({ keyboardOpen: true, fits: false }),
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

// ---- mouse and keyboard mode, on a desktop ----

const desktop = {
  ...DESKTOP,
  mode: "mouse" as const,
  screen: { x: 0, y: 0, width: DESKTOP.width, height: DESKTOP.height },
  cursor: null,
  buttons: buttons({ keyboard: false, away: true }),
};

/** Only the clone's screen: the buttons slid up out of the window. */
export const Desktop: Story = { args: { ...desktop } };

/** The mode just started, or the mouse is at the window's top edge: the buttons are in (no
 *  keyboard button); at the start, the note says how to reach them and the pages. */
export const DesktopButtonsIn: Story = {
  args: {
    ...desktop,
    buttons: buttons({ keyboard: false }),
    notes: { ...notes, hint: "The top edge shows the buttons, Ctrl Alt Space the pages" },
  },
};

/** First open of the site with a layout of three monitors: this window shows monitor 0, and
 *  the button at the left opens the other two. */
export const DesktopMonitorsToOpen: Story = {
  args: { ...desktop, buttons: buttons({ keyboard: false, monitors: { missing: [1, 2], onOpen: fn() } }) },
};

/** The chord showed the pages, at the right, with the buttons above them (no keyboard button)
 *  and the button that moves them to a window of their own. */
export const DesktopPages: Story = {
  args: {
    ...desktop,
    pages: pages({ open: true, onPopOut: fn() }),
    buttons: buttons({ pagesOpen: true, keyboard: false }),
  },
};

/** The Edit layout dialog is open in another window: this one shows its monitor's number. */
export const DesktopIdentify: Story = {
  args: { ...desktop, identify: 1 },
};

/** A long press (or a right click) on Fit: the Edit layout dialog over the pages. */
export const DesktopLayoutDialog: Story = {
  args: {
    ...desktop,
    pages: pages({ open: true, onPopOut: fn() }),
    buttons: buttons({ pagesOpen: true, keyboard: false }),
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
      />
    ),
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
          fits,
          onPages: () => setOpen((o) => !o),
          onKeyboard: () => !docked && setShown((s) => !s),
          onKeyboardDock: () => {
            setDocked((d) => !d);
            setShown(false);
          },
          onFit: () => setFits(true),
        })}
      />
    );
  },
};

/** The mouse at the display's top edge brings the buttons in; below them it sends them away.
 *  The button at the bottom left stands for Ctrl+Alt+Space: it shows or hides the pages. */
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
            const top = e.clientY - e.currentTarget.getBoundingClientRect().top;
            if (top <= 2) setPeek(true);
            else if (top > 84) setPeek(false);
          }}
        >
          <NgScreenView
            {...args}
            pages={pages({ open, tab, onTab: setTab, content: pageFor(tab), onPopOut: fn() })}
            buttons={buttons({
              pagesOpen: open,
              keyboard: false,
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
          Ctrl Alt Space
        </button>
      </>
    );
  },
};
