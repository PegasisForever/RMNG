import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { ChangeAccountModalView } from "./ChangeAccountModalView";
import { makeClaudeAccounts, makeCloneGroups, makeCodexGroups } from "./__fixtures__/accounts";
import { makeCloneDualProvider, makeCloneWorking } from "./__fixtures__/clones";

/** The two lists the pickers take, split out of the flat both-provider list the way the page
 *  splits it. */
function makeAccountLists() {
  const accounts = makeClaudeAccounts();
  return {
    accounts: accounts.filter((a) => a.provider !== "codex"),
    codexAccounts: accounts.filter((a) => a.provider === "codex"),
  };
}

const meta = {
  title: "Clone/Components/ChangeAccountModalView",
  component: ChangeAccountModalView,
  parameters: { layout: "fullscreen" },
  args: {
    cloneName: makeCloneWorking().displayName ?? makeCloneWorking().id,
    ...makeAccountLists(),
    groups: makeCloneGroups(),
    codexGroups: makeCodexGroups(),
    claudeValue: "alex@example.com",
    codexValue: "none",
    busy: false,
    onClaudeValueChange: fn(),
    onCodexValueChange: fn(),
    onClose: fn(),
    onSubmit: fn(),
  },
} satisfies Meta<typeof ChangeAccountModalView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A clone pinned to one Claude account, with Codex configured on the rig — so both pickers
 *  show and the heading says "Accounts" rather than naming one provider. */
export const BothProviders: Story = {};

/** A clone bound to a pool instead of an account. The server keeps it on one member until
 *  that member exhausts, then moves it to the least-used one. */
export const BoundToPool: Story = {
  args: { claudeValue: "group:pooled", codexValue: "group:team" },
};

/** No Codex accounts and no Codex pools configured. The second picker disappears and the
 *  heading narrows to the one provider this rig actually has. */
export const ClaudeOnly: Story = {
  args: { codexAccounts: [], codexGroups: [], codexValue: "none" },
};

/** A clone holding both providers at once, which is what the sub-clone helpers usually look
 *  like. */
export const DualProvider: Story = {
  args: {
    cloneName: makeCloneDualProvider().displayName ?? makeCloneDualProvider().id,
    claudeValue: "alex@example.com",
    codexValue: "group:team",
  },
};

/** The swap is in flight. Apply holds its label and takes no second click; Cancel stays live,
 *  because the page owns the call and closing does not cancel it. */
export const Applying: Story = {
  args: { busy: true },
};

/** The dialog wired to local state: both pickers really change, and Apply locks for a beat
 *  the way the page's swap call does. */
export const Interactive: Story = {
  render: function Render(args) {
    const [claudeValue, setClaudeValue] = useState(args.claudeValue);
    const [codexValue, setCodexValue] = useState(args.codexValue);
    const [busy, setBusy] = useState(false);
    return (
      <ChangeAccountModalView
        {...args}
        claudeValue={claudeValue}
        codexValue={codexValue}
        busy={busy}
        onClaudeValueChange={(next) => {
          setClaudeValue(next);
          args.onClaudeValueChange(next);
        }}
        onCodexValueChange={(next) => {
          setCodexValue(next);
          args.onCodexValueChange(next);
        }}
        onSubmit={() => {
          setBusy(true);
          args.onSubmit();
          window.setTimeout(() => setBusy(false), 1200);
        }}
      />
    );
  },
};
