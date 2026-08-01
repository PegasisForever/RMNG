// A sample BlockNote document, so the notes pane has real content without touching
// /api/notes. Built fresh on each call because the editor mounts against the array it is
// handed and treats it as its own.

import type { PartialBlock } from "@blocknote/core";

export function makeNotesBlocks(): PartialBlock[] {
  return [
    { type: "heading", props: { level: 2 }, content: "Sidebar redesign" },
    {
      type: "paragraph",
      content:
        "The clone rows carry three separate numbers (CPU, memory, tokens) and they currently compete for the same line. Give tokens their own row.",
    },
    { type: "bulletListItem", content: "Fixed-width metric labels so the arrows line up" },
    { type: "bulletListItem", content: "Provider logo shrinks to 12px in the usage rows" },
    { type: "checkListItem", props: { checked: true }, content: "Count tokens from the agent logs" },
    {
      type: "checkListItem",
      props: { checked: false },
      content: "Roll sub clone activity up to the parent",
    },
    { type: "paragraph", content: "" },
  ];
}

export const notesBlocks: PartialBlock[] = makeNotesBlocks();
