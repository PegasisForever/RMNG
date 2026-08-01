// Story-to-story navigation for the Storybook stories.
//
// Opening an overlay (a modal, the settings panel, the ticket drawer) is navigation, so a
// story wires it to the overlay's own story rather than rendering it inline. That keeps the
// page story showing the page and gives every overlay one place where all of its states live.
//
// Why not `linkTo` from the addon directly: `linkTo(title, ExportName)` resolves the export
// name through the manager, which does not camel-case-split it back into a story id, so a
// link to `AgentWorking` silently does nothing. Splitting the name here and navigating by
// explicit story id is the same call the addon would make, minus the lookup that drops it.

import { navigate } from "@storybook/addon-links";
import { storyNameFromExport, toId } from "storybook/internal/csf";

/** Jump to another story now, by its meta `title` and its export name (`"AgentWorking"`) or
 *  its display name (`"Agent working"`). Use this when the destination depends on the
 *  callback's own argument; otherwise use `linkToStory`. */
export function navigateToStory(title: string, exportName: string): void {
  const name = exportName.includes(" ") ? exportName : storyNameFromExport(exportName);
  navigate({ storyId: toId(title, name) });
}

/** A callback that jumps to another story when it fires. Drops straight into story args in
 *  place of `fn()` wherever the prop is a navigation seam:
 *
 *  ```ts
 *  onOpenSettings: linkToStory("Settings/SettingsPanel", "Default")
 *  ```
 *
 *  The handler ignores its arguments, so it satisfies a callback prop of any signature. */
export function linkToStory(title: string, exportName: string): (...args: unknown[]) => void {
  return () => navigateToStory(title, exportName);
}
