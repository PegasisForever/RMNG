// Story-to-story navigation for the Storybook stories.
//
// Opening an overlay (a modal, the settings panel, the ticket drawer) is navigation, so a
// story wires it to the overlay's own story rather than rendering it inline. That keeps the
// page story showing the page and gives every overlay one place where all of its states live.
//
// Two exports, and the whole point of the pair is which one goes where:
//
//   in args:      onOpenSettings: makeStoryLink("Settings/Components/SettingsPanel", "Default")
//   in a handler: onSelectTicket: (t) => goToStory("Board/Components/TicketPanel", nameFor(t))
//
// Put `goToStory` in args by mistake and the preview jumps the moment the module is evaluated,
// before a single story has rendered. TypeScript catches that for a required callback prop and
// misses it for an optional one, so the names carry the difference instead: one reads as an
// action to perform, the other reads as a value to hand over.
//
// Why not `linkTo` from the addon directly: `linkTo(title, ExportName)` resolves the export
// name through the manager, which does not camel-case-split it back into a story id, so a
// link to `AgentWorking` silently does nothing. Splitting the name here and navigating by
// explicit story id is the same call the addon would make, minus the lookup that drops it.

import { navigate } from "@storybook/addon-links";
import { storyNameFromExport, toId } from "storybook/internal/csf";

/** Jump to another story NOW, by its meta `title` and its export name (`"AgentWorking"`) or
 *  its display name (`"Agent working"`).
 *
 *  Call this inside a handler. It navigates on the spot, so a bare `onOpenX: goToStory(...)`
 *  in story args fires during module evaluation instead of on the click. Use it when the
 *  destination depends on the callback's own argument. Otherwise use `makeStoryLink`. */
export function goToStory(title: string, exportName: string): void {
  const name = exportName.includes(" ") ? exportName : storyNameFromExport(exportName);
  navigate({ storyId: toId(title, name) });
}

/** Build a callback that jumps to another story when it fires.
 *
 *  This is the one that goes in story args, in place of `fn()`, wherever the prop is a
 *  navigation seam:
 *
 *  ```ts
 *  onOpenSettings: makeStoryLink("Settings/Components/SettingsPanel", "Default")
 *  ```
 *
 *  The handler ignores its arguments, so it satisfies a callback prop of any signature. */
export function makeStoryLink(title: string, exportName: string): (...args: unknown[]) => void {
  return () => goToStory(title, exportName);
}
