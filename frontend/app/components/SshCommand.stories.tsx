import type { Meta, StoryObj } from "@storybook/react-vite";

import { buildSshCommand } from "~/lib/ssh";

/** Presentational-only wrapper so Storybook can render `buildSshCommand`'s pure output.
 *  Visually verifies it matches the CLI's `build_ssh_command` char-for-char (see
 *  `crates/cli/src/commands.rs`'s `ssh_command_is_the_inline_jump_one_liner` test, which
 *  asserts the identical arguments below). */
function SshCommandPreview({
  publicHost,
  bastionPort,
  cloneId,
}: {
  publicHost: string;
  bastionPort: number;
  cloneId: string;
}) {
  return (
    <pre className="max-w-xl overflow-x-auto rounded border border-slate-300 bg-slate-50 p-3 font-mono text-xs text-slate-800 dark:border-slate-600 dark:bg-slate-900 dark:text-slate-100">
      {buildSshCommand(publicHost, bastionPort, cloneId)}
    </pre>
  );
}

const meta = {
  title: "Sidebar/SshCommand",
  component: SshCommandPreview,
  parameters: { layout: "padded" },
} satisfies Meta<typeof SshCommandPreview>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The copy-paste one-liner "Copy SSH command" places on the clipboard, and what
 *  `rmng ssh w-cp-claude` prints — same arguments as the CLI's unit test. */
export const Default: Story = {
  args: { publicHost: "rmng.example.com", bastionPort: 2222, cloneId: "w-cp-claude" },
};

/** No public-host override configured — the UI falls back to `window.location.hostname`
 *  before calling `buildSshCommand`, so the string itself is unaffected either way. */
export const CustomClone: Story = {
  args: { publicHost: "10.0.0.15", bastionPort: 2222, cloneId: "pega-dev-123" },
};
