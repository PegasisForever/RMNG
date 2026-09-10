import type { Meta, StoryObj } from "@storybook/react-vite";
import { StrictMode, useState, type ComponentProps } from "react";
import { fn } from "storybook/test";

import { SettingsGroupsEditor } from "./SettingsGroupsEditor";
import { accountsNow, makeClaudeAccounts } from "./__fixtures__/accounts";
import { makeSettingsDraft } from "./__fixtures__/appConfig";

/** The editor sits in the panel's body, so the story gives it the same width. */
function Frame({ children }: { children: React.ReactNode }) {
  return <div className="w-[38rem] max-w-full p-4">{children}</div>;
}

/** Both providers' rows, the way the panel hands them to the editor. */
function allAccounts() {
  return makeClaudeAccounts(accountsNow);
}

const CLAUDE_HINT = "Import some accounts first to add them to a group.";

function InteractiveEditor(args: ComponentProps<typeof SettingsGroupsEditor>) {
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
}

const meta = {
  title: "Settings/Components/SettingsGroupsEditor",
  component: SettingsGroupsEditor,
  parameters: { layout: "centered" },
  args: {
    groups: makeSettingsDraft().groups,
    accounts: allAccounts(),
    noAccountsHint: CLAUDE_HINT,
    onChange: fn(),
    onImportAccount: fn(),
  },
  render: (args) => <InteractiveEditor {...args} />,
} satisfies Meta<typeof SettingsGroupsEditor>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Two pools over mixed Claude + Codex members. An account can sit in several pools; the
 *  pools are how a clone's binding is resolved, not a partition. Drag a row across pools
 *  to move it, or inside its pool to reorder. */
export const Mixed: Story = { args: { groups: makeSettingsDraft().groups } };

/** An imported account claimed by no pool. Saving deletes it, so it is listed with the
 *  reason instead of silently vanishing on save. */
export const UngroupedWarning: Story = {
  args: {
    groups: [{ name: "solo", accounts: ["alex@example.com"] }],
    accounts: allAccounts(),
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
    groups: [],
    accounts: [],
    noAccountsHint: CLAUDE_HINT,
  },
};

/** A half-typed pool, which is exactly what a save drops: a blank name is not stored as an
 *  unnamed pool. */
export const UnnamedRow: Story = {
  args: { groups: [{ name: "", accounts: [] }] },
};

/** All stories accept edits; this is the standard populated example. */
export const Interactive: Story = {
  args: { groups: makeSettingsDraft().groups },
};

/** Regression fixture: shared membership, an empty destination, and a scrollable body.
 * The visible draft counter makes it easy to check that hover never writes a change. */
export const DragAndDrop: Story = {
  args: {
    groups: [
      { name: "pooled", accounts: ["alex@example.com", "sam@example.com"] },
      { name: "team", accounts: ["alex@openai.com"] },
      { name: "shared", accounts: ["alex@example.com"] },
      { name: "empty", accounts: [] },
    ],
  },
  render: function Render(args) {
    const [groups, setGroups] = useState(args.groups);
    const [changes, setChanges] = useState(0);
    const reset = () => {
      setGroups(structuredClone(args.groups));
      setChanges(0);
    };
    return (
      <StrictMode>
        <Frame>
          <div
            className="h-[32rem] overflow-y-auto border p-2"
            data-editor-scroll
          >
            <SettingsGroupsEditor
              {...args}
              groups={groups}
              onChange={(next) => {
                setGroups(next);
                setChanges((count) => count + 1);
                args.onChange(next);
              }}
            />
          </div>
          <p className="mt-2 text-xs">
            Draft changes: <output data-draft-changes>{changes}</output>
          </p>
          {/* Pointer-down also permits an external replacement while a drag suppresses clicks. */}
          <button type="button" onPointerDown={reset} onClick={reset}>
            Reset example
          </button>
          <details>
            <summary>Draft</summary>
            <pre data-draft-value>{JSON.stringify(groups, null, 2)}</pre>
          </details>
        </Frame>
      </StrictMode>
    );
  },
};
