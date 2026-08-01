import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import type { ReactNode } from "react";
import { fn } from "storybook/test";

import { NotesEditorView } from "./NotesEditorView";
import { makeNotesBlocks } from "./__fixtures__/notes";

/** The pane fills the notes card in the shell's side column, so the story gives it one of the
 *  same shape rather than letting it size to its contents. The document scrolls inside it. */
function Frame({ children }: { children: ReactNode }) {
  return (
    <div className="flex h-[34rem] w-[26rem] flex-col overflow-hidden rounded-2xl border border-slate-900/10 bg-white shadow-xl dark:border-white/10 dark:bg-slate-900">
      <h2 className="shrink-0 truncate px-4 pt-3 text-sm font-semibold text-slate-900 dark:text-slate-100">
        pega-we-142
      </h2>
      <div className="min-h-0 flex-1 overflow-y-auto py-2">{children}</div>
    </div>
  );
}

/** Pasting or dropping an image really shows it, with no server behind it: the browser hands
 *  back a URL for the file already in memory. The live pane posts to /api/upload instead. */
const previewUpload = async (file: File) => URL.createObjectURL(file);

const meta = {
  title: "Clone/Components/NotesEditorView",
  component: NotesEditorView,
  parameters: { layout: "centered" },
  args: {
    initialContent: undefined,
    onChange: fn(),
    uploadFile: previewUpload,
  },
  render: (args) => (
    <Frame>
      <NotesEditorView {...args} />
    </Frame>
  ),
} satisfies Meta<typeof NotesEditorView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A clone whose notes nobody has written yet. BlockNote opens on a single empty paragraph,
 *  which is also what a document that failed to load falls back to. */
export const Empty: Story = {};

/** A note with something in it: headings, a list, and check items, which is the mix the
 *  toolbar and the drag handles have to sit around. */
export const Loaded: Story = {
  // The array is built per mount, not per module: BlockNote mounts against the array it is
  // handed and treats it as its own, so a shared one would come back edited by whichever
  // story ran first. `useState`'s lazy initializer is what makes it per mount.
  render: function Render(args) {
    const [initialContent] = useState(makeNotesBlocks);
    return (
      <Frame>
        <NotesEditorView {...args} initialContent={initialContent} />
      </Frame>
    );
  },
};

/** The pane wired to local state instead of /api/notes: every edit lands in the readout under
 *  the editor, which is where the container's debounce would pick it up. Paste an image and
 *  the upload stub resolves it straight into the document. */
export const Interactive: Story = {
  render: function Render(args) {
    const [initialContent] = useState(makeNotesBlocks);
    const [blocks, setBlocks] = useState<unknown[] | null>(null);
    return (
      <Frame>
        <NotesEditorView
          {...args}
          initialContent={initialContent}
          onChange={(next) => {
            setBlocks(next);
            args.onChange(next);
          }}
        />
        <p className="px-4 pb-3 pt-1 font-mono text-[11px] text-slate-400 dark:text-slate-500">
          {blocks === null ? "no edit yet" : `onChange: ${blocks.length} blocks`}
        </p>
      </Frame>
    );
  },
};
