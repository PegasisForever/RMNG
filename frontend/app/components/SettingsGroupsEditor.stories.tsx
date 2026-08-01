import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { SettingsGroupsEditor } from "./SettingsGroupsEditor";
import { accountsNow, makeClaudeAccounts } from "./__fixtures__/accounts";
import { makeSettingsDraft } from "./__fixtures__/appConfig";
import { orderedAccounts } from "~/lib/settingsDraft";

/** The editor sits in the panel's body, so the story gives it the same width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] p-4">{children}</div>;
}

/** One provider's emails, derived the way the panel derives them. */
function emails(provider: "claude" | "codex") {
  return orderedAccounts(makeClaudeAccounts(accountsNow), {})[provider].map((a) => a.email);
}

const CLAUDE_HINT = "Import some accounts first to add them to a group.";
const CODEX_HINT = "Import some Codex accounts first to add them to a group.";

const meta = {
  title: "Settings/Components/SettingsGroupsEditor",
  component: SettingsGroupsEditor,
  parameters: { layout: "centered" },
  args: {
    groups: makeSettingsDraft().claudeGroups,
    accountEmails: emails("claude"),
    noAccountsHint: CLAUDE_HINT,
    onChange: fn(),
  },
  render: (args) => (
    <Frame>
      <SettingsGroupsEditor {...args} />
    </Frame>
  ),
} satisfies Meta<typeof SettingsGroupsEditor>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Two Claude pools over the same two accounts. An account can sit in several pools; the
 *  pools are how a clone's binding is resolved, not a partition. */
export const Claude: Story = { args: { groups: makeSettingsDraft().claudeGroups } };

/** The Codex twin, over the independent Codex pool. A clone binds one pool of each provider,
 *  which is why the two lists never merge. */
export const Codex: Story = {
  args: {
    groups: makeSettingsDraft().codexGroups,
    accountEmails: emails("codex"),
    noAccountsHint: CODEX_HINT,
  },
};

/** No pools configured. Every clone then falls through to the server's own account chain. */
export const Empty: Story = {
  args: { groups: [] },
};

/** A pool with nothing to put in it. The hint names the provider, because importing a Claude
 *  account does not help a Codex pool. */
export const NoAccountsImported: Story = {
  args: {
    groups: makeSettingsDraft().codexGroups,
    accountEmails: [],
    noAccountsHint: CODEX_HINT,
  },
};

/** A half-typed pool, which is exactly what a save drops: a blank name is not stored as an
 *  unnamed pool. */
export const UnnamedRow: Story = {
  args: { groups: [{ name: "", accounts: [] }] },
};

/** Wired to local state: renaming, ticking a member and adding a pool all take effect. */
export const Interactive: Story = {
  args: { groups: makeSettingsDraft().claudeGroups },
  render: function Render(args) {
    const [groups, setGroups] = useState(args.groups);
    return (
      <Frame>
        <SettingsGroupsEditor
          {...args}
          groups={groups}
          onChange={(next) => {
            setGroups(next);
            args.onChange(next);
          }}
        />
      </Frame>
    );
  },
};
