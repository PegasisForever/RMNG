// Unsent composer text, one draft per clone, held in memory for the tab's lifetime.
//
// `ChatPanel` is mounted with `key={clone.id}`, so selecting another clone unmounts it and
// takes its `input` state with it. The drafts live out here, outside the component tree, so
// text typed for one clone is still in the box after a detour through another.
//
// Client-only by design: a draft is scratch text the operator has not sent yet, and the
// server holds nothing about it. It is also not persisted to storage, so a reload starts
// clean and no draft outlives the clone it was written for.
const drafts = new Map<string, string>();

/** The draft for `cloneId`, or `""` when there is none. */
export function getDraft(cloneId: string): string {
  return drafts.get(cloneId) ?? "";
}

/** Store `text` as the draft for `cloneId`. Empty text drops the entry instead of keeping
 *  a blank one, so an emptied box leaves nothing behind. */
export function setDraft(cloneId: string, text: string): void {
  if (text) drafts.set(cloneId, text);
  else drafts.delete(cloneId);
}

/** Drop every draft. Test hook. */
export function clearDrafts(): void {
  drafts.clear();
}
