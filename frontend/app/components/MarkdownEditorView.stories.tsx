import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { MarkdownEditorView } from "./MarkdownEditorView";

/** The field sits in the clone dialog's Description slot, which gives it a border, a floor
 *  height, and a focus ring. Reproduced here so the editor is judged at the size it ships at. */
function Frame({ children }: { children: ReactNode }) {
  return (
    <div className="w-[30rem] text-xs font-medium text-slate-500 dark:text-slate-400">
      Description
      <div className="mt-1 min-h-[6.5rem] rounded-md border border-slate-300 py-2 text-sm font-normal focus-within:border-emerald-500 dark:border-slate-600">
        {children}
      </div>
    </div>
  );
}

/** Pasting or dropping an image really shows it, with no server behind it: the browser hands
 *  back a URL for the file already in memory. The live field posts to /api/upload instead,
 *  and the server re-hosts the result in Linear before the ticket is created. */
const previewUpload = async (file: File) => URL.createObjectURL(file);

const meta = {
  title: "Clone/Components/MarkdownEditorView",
  component: MarkdownEditorView,
  parameters: { layout: "centered" },
  args: {
    onChange: fn(),
    uploadFile: previewUpload,
    placeholder: "What needs doing — paste images, format freely",
  },
  render: (args) => (
    <Frame>
      <MarkdownEditorView {...args} />
    </Frame>
  ),
} satisfies Meta<typeof MarkdownEditorView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** How the dialog always opens it: empty, on the placeholder. The field has no way to be
 *  handed a starting document, so this is the only state it mounts in. */
export const Default: Story = {};

/** No placeholder configured, which leaves BlockNote's own prompt in its place. */
export const NoPlaceholder: Story = {
  args: { placeholder: undefined },
};

/** The field wired to local state: the markdown it reports on every keystroke is printed
 *  underneath, which is the exact string the dialog would send as the ticket body. */
export const Interactive: Story = {
  render: function Render(args) {
    const [markdown, setMarkdown] = useState("");
    return (
      <Frame>
        <MarkdownEditorView
          {...args}
          onChange={(next) => {
            setMarkdown(next);
            args.onChange(next);
          }}
        />
        <pre className="mt-2 max-h-40 overflow-auto border-t border-slate-200 px-3 pt-2 font-mono text-[11px] whitespace-pre-wrap text-slate-500 dark:border-slate-700 dark:text-slate-400">
          {markdown || "(empty)"}
        </pre>
      </Frame>
    );
  },
};
