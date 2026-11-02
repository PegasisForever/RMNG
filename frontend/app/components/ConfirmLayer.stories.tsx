import type { Meta, StoryObj } from "@storybook/react-vite";
import { useEffect } from "react";

import { FakeDesktop } from "~/desk/__fixtures__/desktop";

import { ConfirmLayer, type Question, ask } from "./ConfirmLayer";

/** The layer over a clone's desktop, asked `question` once it is on the page. */
function Asked({ question }: { question: Question }) {
  useEffect(() => {
    void ask(question);
  }, [question]);
  return (
    <div className="relative h-screen w-screen overflow-hidden bg-black">
      <FakeDesktop />
      <ConfirmLayer />
    </div>
  );
}

const meta = {
  title: "Modals/Components/ConfirmLayer",
  component: Asked,
} satisfies Meta<typeof Asked>;
export default meta;
type Story = StoryObj<typeof meta>;

/** A question whose yes destroys something: the yes is red text. */
export const Danger: Story = {
  args: {
    question: {
      title: "Delete pega-we-151?",
      body: "Its container and its home folder are deleted. This cannot be undone.",
      ok: "Delete",
      danger: true,
    },
  },
};

/** A question whose yes goes ahead: the yes is the green button. */
export const GoAhead: Story = {
  args: { question: { title: "Update the control-server?", body: "It restarts with the new build.", ok: "Update" } },
};

/** A message, with one button. */
export const Message: Story = {
  args: {
    question: {
      title: "The browser blocked 2 monitor windows",
      body: 'Click the blocked pop-up icon at the right of the address bar, choose "Always allow pop-ups", then open them again.',
      ok: "OK",
      cancel: null,
    },
  },
};
