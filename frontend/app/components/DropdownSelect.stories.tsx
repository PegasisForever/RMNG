// The app's dropdown in its states: a plain list, headers plus a disabled row, an empty
// list and a blank trigger. Open menus are exercised by clicking the trigger (see the
// Interactive story); every state below is a prop combination.
import { useState } from "react";

import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { FakeDesktop } from "~/desk/__fixtures__/desktop";
import { FIELD } from "~/desk/control/ui";

import type { DropdownRow } from "./DropdownSelect";
import { DropdownSelect } from "./DropdownSelect";

const field = `w-full ${FIELD} px-3 py-2 text-sm font-normal`;

function Controlled({
  rows,
  initial,
  label,
}: {
  rows: DropdownRow[];
  initial: string;
  label: string;
}) {
  const [value, setValue] = useState(initial);
  return (
    <div className="w-72">
      <DropdownSelect
        rows={rows}
        value={value}
        onChange={(v) => {
          setValue(v);
          fn()(v);
        }}
        label={label}
        className={field}
      />
    </div>
  );
}

const meta = {
  title: "Clone/Components/DropdownSelect",
  component: DropdownSelect,
  parameters: { layout: "fullscreen" },
  // Over a clone's screen, where the lists open: the list is glass, the field flat on the page
  decorators: [
    (Story) => (
      <div className="relative flex h-screen w-screen items-start justify-center overflow-hidden bg-black pt-24">
        <FakeDesktop />
        <div className="relative">
          <Story />
        </div>
      </div>
    ),
  ],
} satisfies Meta<typeof DropdownSelect>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A plain list, preselected. */
const plainRows: DropdownRow[] = [
  { value: "pega-we-142", label: "pega-we-142" },
  { value: "pega-dev-88", label: "pega-dev-88" },
  { value: "pega-ops-7", label: "pega-ops-7" },
];

export const Plain: Story = {
  args: { rows: plainRows, value: "pega-we-142", onChange: fn() },
  render: () => (
    <Controlled
      rows={[
        { value: "pega-we-142", label: "pega-we-142" },
        { value: "pega-dev-88", label: "pega-dev-88" },
        { value: "pega-ops-7", label: "pega-ops-7" },
      ]}
      initial="pega-we-142"
      label="Source clone to fork"
    />
  ),
};

/** Headers, a disabled row, and usage-suffixed labels, as the account pickers use them. */
const headerRows: DropdownRow[] = [
  { value: "", label: "Follow group" },
  { value: "auto", label: "Auto (all accounts)" },
  { header: "Accounts" },
  { value: "alex@example.com", label: "alex@example.com — 5h 42%" },
  {
    value: "sam@example.com",
    label: "sam@example.com (exhausted)",
    disabled: true,
  },
];

export const Headers: Story = {
  args: { rows: headerRows, value: "", onChange: fn() },
  render: () => (
    <Controlled
      rows={[
        { value: "", label: "Follow group" },
        { value: "auto", label: "Auto (all accounts)" },
        { header: "Accounts" },
        { value: "alex@example.com", label: "alex@example.com — 5h 42%" },
        {
          value: "sam@example.com",
          label: "sam@example.com (exhausted)",
          disabled: true,
        },
      ]}
      initial=""
      label="Claude account"
    />
  ),
};

/** Nothing to pick yet (a still-loading lookup): the trigger is blank and the menu opens
 *  empty. */
export const Empty: Story = {
  args: { rows: [], value: "", onChange: fn() },
  render: () => <Controlled rows={[]} initial="" label="Assignee" />,
};

/** Blank trigger: the value matches no option (a picker waiting for its preset, like the
 *  clone dialog's source). The trigger must stand at
 *  exactly one text line, not collapse to the chevron. */
export const Blank: Story = {
  args: { rows: plainRows, value: "", onChange: fn() },
  render: () => (
    <Controlled rows={plainRows} initial="" label="Source clone to fork" />
  ),
};
