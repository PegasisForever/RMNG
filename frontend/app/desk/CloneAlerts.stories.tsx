import type { Meta, StoryObj } from "@storybook/react-vite";
import { useRef, useState } from "react";
import { fn } from "storybook/test";

import type { CloneAlert } from "~/lib/useCloneNotifications";

import { FakeDesktop } from "./__fixtures__/screen";
import { CloneAlerts } from "./CloneAlerts";
import { BUTTON } from "./control/ui";

const idle: CloneAlert = { id: "pega-we-142", name: "Fix the sign-in redirect", offline: false, at: 1 };
const offline: CloneAlert = { id: "pega-dev-9", name: "pega-dev-9", offline: true, at: 2 };
const long: CloneAlert = {
  id: "pega-we-150",
  name: "Move the account binding behind one reading for every clone and its sub clones",
  offline: false,
  at: 3,
};

const meta = {
  title: "Desk/CloneAlerts",
  component: CloneAlerts,
  parameters: { layout: "fullscreen" },
  // In the bottom-right corner of a clone's desktop, as in monitor 0's window: light or dark
  // by the Theme
  decorators: [
    (Story) => (
      <div className="relative h-screen overflow-hidden">
        <FakeDesktop />
        <div className="pointer-events-none absolute inset-2 flex flex-col items-end justify-end">
          <Story />
        </div>
      </div>
    ),
  ],
  args: { alerts: [idle], onOpen: fn(), onClose: fn() },
} satisfies Meta<typeof CloneAlerts>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A clone stopped working and is idle. A click on the alert shows the clone. */
export const Idle: Story = {};

/** A clone went offline (its container stopped). */
export const Offline: Story = { args: { alerts: [offline] } };

/** Three clones stopped one after the other: the newest is at the bottom. A long name is cut. */
export const Several: Story = { args: { alerts: [offline, long, idle] } };

/** Press "Stop a clone" to see an alert slide in; close one, or click it, to see it slide out. */
export const Live: Story = {
  args: { alerts: [] },
  render: function Render(args) {
    const [alerts, setAlerts] = useState<CloneAlert[]>([]);
    const n = useRef(0);
    const add = () => {
      n.current += 1;
      const i = n.current;
      setAlerts((list) => [...list, { id: `pega-we-${140 + i}`, name: `pega-we-${140 + i}`, offline: i % 3 === 0, at: i }]);
    };
    const remove = (id: string) => setAlerts((list) => list.filter((a) => a.id !== id));
    return (
      <>
        <div className="pointer-events-auto fixed left-4 top-10">
          <button type="button" className={`${BUTTON} px-3 py-1.5 text-sm`} onClick={add}>
            Stop a clone
          </button>
        </div>
        <CloneAlerts
          {...args}
          alerts={alerts}
          onOpen={(id) => {
            args.onOpen(id);
            remove(id);
          }}
          onClose={(id) => {
            args.onClose(id);
            remove(id);
          }}
        />
      </>
    );
  },
};
