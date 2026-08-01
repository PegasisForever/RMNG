import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { EnvChecklistView } from "./EnvChecklistView";
import { makeEnvCheckRow, makeEnvRows } from "./__fixtures__/setupEnv";

/** The checklist sits in the setup wizard's body, so the story gives it that column width
 *  rather than letting the rows run the full page. */
function Frame({ children }: { children: ReactNode }) {
  return <div className="w-[34rem]">{children}</div>;
}

const meta = {
  title: "Setup/Components/EnvChecklistView",
  component: EnvChecklistView,
  parameters: { layout: "centered" },
  args: {
    rows: makeEnvRows(),
    loading: false,
    error: null,
    onRetry: fn(),
  },
  render: (args) => (
    <Frame>
      <EnvChecklistView {...args} />
    </Frame>
  ),
} satisfies Meta<typeof EnvChecklistView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A host that is ready. Every required check passes, so the wizard's Next button unlocks. */
export const AllPass: Story = {};

/** The first probe has not answered yet. Nothing is known, so the list is absent rather than
 *  empty. */
export const Running: Story = {
  args: { rows: null, loading: true },
};

/** A required check failed: red, and the line underneath says setup cannot continue. This is
 *  the state that blocks the wizard. */
export const RequiredFailing: Story = {
  args: {
    rows: [
      makeEnvCheckRow({
        ok: false,
        detail: "connect /var/run/docker.sock: permission denied",
      }),
      ...makeEnvRows().slice(1),
    ],
  },
};

/** Only an advisory check failed: amber, tagged optional, and nothing is blocked. Clones will
 *  run, they just will not get the hardware path. */
export const AdvisoryWarning: Story = {
  args: {
    rows: [
      ...makeEnvRows().slice(0, 3),
      makeEnvCheckRow({
        id: "kvm",
        label: "Nested virtualization",
        ok: false,
        detail: "/dev/kvm missing — clones fall back to software rendering",
        required: false,
      }),
    ],
  },
};

/** The probe itself failed, which is different from a check failing: nothing was measured, so
 *  no rows are drawn and Retry is the only thing to do. */
export const ProbeError: Story = {
  args: { rows: null, error: "502 Bad Gateway" },
};

/** Wired to local state: Retry really re-runs, and the run lands on a host that has since been
 *  fixed, so the failing check goes green. */
export const Interactive: Story = {
  render: function Render(args) {
    const [rows, setRows] = useState(args.rows);
    const [loading, setLoading] = useState(false);
    return (
      <Frame>
        <EnvChecklistView
          {...args}
          rows={rows}
          loading={loading}
          onRetry={() => {
            setLoading(true);
            args.onRetry();
            window.setTimeout(() => {
              setRows(makeEnvRows());
              setLoading(false);
            }, 900);
          }}
        />
      </Frame>
    );
  },
  args: {
    rows: [
      makeEnvCheckRow({ ok: false, detail: "connect /var/run/docker.sock: permission denied" }),
      ...makeEnvRows().slice(1),
    ],
  },
};
