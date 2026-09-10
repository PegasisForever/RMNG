import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { SettingsAccountList } from "./SettingsAccountList";
import { accountsNow, makeClaudeAccounts } from "./__fixtures__/accounts";
import { makeStoryLink } from "./__fixtures__/storyLinks";
import { orderedAccounts } from "~/lib/settingsDraft";

/** The list sits in the panel's body, so the story gives it the same width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

/** This provider's rows in the operator's saved order, derived exactly the way the panel
 *  derives them, so a story cannot show a list the panel could not produce. */
function rows(provider: "claude" | "codex") {
  return orderedAccounts(makeClaudeAccounts(accountsNow), {})[provider];
}

const meta = {
  title: "Settings/Components/SettingsAccountList",
  component: SettingsAccountList,
  parameters: { layout: "centered" },
  args: {
    accounts: rows("claude"),
    onDelete: fn(),
    onReorder: fn(),
    onReplace: makeStoryLink("Settings/Components/ImportAccountModalView", "Replacing"),
  },
  render: (args) => (
    <Frame>
      <SettingsAccountList {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SettingsAccountList>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The Claude list: every imported account, draggable by its grip, each with a delete the
 *  container confirms. Importing lives in the group tree below — every import lands in a
 *  pool directly — so this list offers none. */
export const Claude: Story = {
  args: {
    accounts: rows("claude"),
  },
};

/** The Codex list: the same rows without an import button. Importing is provider-picked
 *  inside the same modal, so a second entry point here would open the same dialog. */
export const Codex: Story = {
  args: { accounts: rows("codex") },
};

/** Nothing imported. The empty state points at where an account comes from, because there is
 *  no browser login: the control-server harvests the token off a clone, picked up from a
 *  pool's import button in the group tree below. */
export const Empty: Story = {
  args: {
    accounts: [],
  },
};

/** Wired to local state, so a drag really reorders. The order is cosmetic and client-side —
 *  the pool is unordered as far as the server is concerned, and it is never sent with the
 *  config patch. */
export const Interactive: Story = {
  args: { accounts: rows("claude") },
  render: function Render(args) {
    const [accounts, setAccounts] = useState(args.accounts);
    return (
      <Frame>
        <SettingsAccountList
          {...args}
          accounts={accounts}
          onReorder={(ids) => {
            setAccounts((prev) =>
              ids.flatMap((id) => {
                const row = prev.find((a) => a.id === id);
                return row ? [row] : [];
              }),
            );
            args.onReorder(ids);
          }}
          onDelete={(email) => {
            setAccounts((prev) => prev.filter((a) => a.email !== email));
            args.onDelete(email);
          }}
        />
      </Frame>
    );
  },
};
