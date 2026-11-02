import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { FakeDesktop } from "~/desk/__fixtures__/desktop";
import { BUTTON, DONE_BUTTON, TEXT, TEXT_2 } from "~/desk/control/ui";

import { ModalShell, ModalSurfaceProvider, type ModalClose } from "./ModalShell";

/** Stand-in for a dialog's own markup. Every dialog puts its content here and reaches the
 *  shell's door out the same way, so the frame can be reviewed without one. */
function SampleDialog({
  title,
  close,
  lines = 3,
}: {
  title: string;
  close: ModalClose;
  lines?: number;
}) {
  return (
    <>
      <h3 className={`shrink-0 text-[15px] font-semibold tracking-tight ${TEXT}`}>
        {title}
      </h3>
      <div className="min-h-0 shrink space-y-2 overflow-y-auto pt-3">
        {Array.from({ length: lines }, (_, i) => (
          <p key={i} className={`text-xs ${TEXT_2}`}>
            A dialog states what it is — how wide, whether Escape may close
            it — and puts its own markup inside.
          </p>
        ))}
      </div>
      <div className="mt-4 flex shrink-0 justify-end gap-2">
        <button
          type="button"
          onClick={() => close()}
          className={`${BUTTON} px-3 py-1.5 text-sm`}
        >
          Cancel
        </button>
        <button
          type="button"
          className={`${DONE_BUTTON} px-4 py-1.5 text-sm font-medium`}
        >
          Apply
        </button>
      </div>
    </>
  );
}

const meta = {
  title: "Modals/Components/ModalShell",
  component: ModalShell,
  parameters: { layout: "fullscreen" },
  args: {
    onExited: fn(),
    children: (close: ModalClose) => (
      <SampleDialog title="Dialog" close={close} />
    ),
  },
  /** Over a clone's screen, as a dialog opens: glass. Cancel and Escape both play the real exit
   *  frames. */
  decorators: [
    (Story) => (
      <div className="relative h-screen w-screen overflow-hidden bg-black">
        <FakeDesktop />
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof ModalShell>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A short form: the account and sign-in dialogs. */
export const Sm: Story = { args: { size: "sm" } };

/** A wider short form: port forwards. */
export const Md: Story = { args: { size: "md" } };

/** A form tall enough to need a scrolling middle, which the dialog pins itself: the rebase
 *  and new-ticket dialogs. */
export const Lg: Story = {
  args: {
    size: "lg",
    children: (close: ModalClose) => (
      <SampleDialog title="Tall dialog" close={close} lines={20} />
    ),
  },
};

/** The two-pane fixed-height overlay: the clone dialog. */
export const Panel: Story = {
  args: {
    size: "panel",
    children: (close: ModalClose) => (
      <div className="flex min-h-0 flex-1 flex-col p-4">
        <SampleDialog title="Panel" close={close} lines={20} />
      </div>
    ),
  },
};

/** An operation is running: Escape is dead, so the dialog cannot be closed out from under
 *  the job it started. The buttons are the dialog's own to disable. */
export const NotDismissible: Story = {
  args: {
    size: "sm",
    dismissible: false,
    children: (close: ModalClose) => (
      <SampleDialog title="Working…" close={close} />
    ),
  },
};

/** In the control panel's own window (no clone's screen under it): solid, in the page's look, on
 *  the window's solid ground. */
export const Solid: Story = {
  args: { size: "sm" },
  decorators: [
    (Story) => (
      <ModalSurfaceProvider value="solid">
        <div className="absolute inset-0 bg-neutral-100 dark:bg-neutral-950">
          <Story />
        </div>
      </ModalSurfaceProvider>
    ),
  ],
};
