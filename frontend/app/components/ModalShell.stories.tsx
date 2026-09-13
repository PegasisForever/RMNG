import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { ModalShell, type ModalClose } from "./ModalShell";

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
      <h3 className="shrink-0 text-sm font-semibold text-slate-900 dark:text-slate-100">
        {title}
      </h3>
      <div className="min-h-0 shrink space-y-2 overflow-y-auto pt-3">
        {Array.from({ length: lines }, (_, i) => (
          <p key={i} className="text-xs text-slate-500 dark:text-slate-400">
            A dialog states what it is — how wide, which rung, whether Escape
            may close it — and puts its own markup inside.
          </p>
        ))}
      </div>
      <div className="mt-4 flex shrink-0 justify-end gap-2">
        <button
          type="button"
          onClick={() => close()}
          className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800"
        >
          Cancel
        </button>
        <button
          type="button"
          className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700"
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
  /** The shell positions itself over the page, so the stories give it a page-coloured one to
   *  sit on rather than a white void. Cancel and Escape both play the real exit frames. */
  decorators: [
    (Story) => (
      <div className="h-screen bg-slate-50 dark:bg-slate-950">
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

/** The two-pane fixed-height overlay: the clone dialog and Settings. */
export const Panel: Story = {
  args: {
    size: "panel",
    children: (close: ModalClose) => (
      <div className="flex min-h-0 flex-1 flex-col p-5">
        <SampleDialog title="Panel" close={close} lines={20} />
      </div>
    ),
  },
};

/** The `over` rung, for a dialog opened from another one. It looks the same alone — what it
 *  buys shows only with a second dialog underneath. */
export const Over: Story = { args: { size: "sm", layer: "over" } };

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
