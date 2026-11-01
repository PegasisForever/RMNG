// The edit shortcuts the page runs itself while the keyboard is locked (the page's fullscreen,
// `~/viewer/input.ts`). On a Mac, Chrome runs Cmd+C, Cmd+X, Cmd+V, Cmd+A, Cmd+Z from its Edit
// menu, and a locked keyboard sends those keys to the page as plain keys instead: nothing copies
// or pastes. So while the page has the keys (the control panel, the side panels, a dialog), it
// does each one itself, on the focused field or the selection:
//
// - copy and cut: the browser's own command, whose copy or cut event an editor (BlockNote) takes
//   as from the menu;
// - paste: the clipboard read (the frame may: `public/shell.html`), then, in an editor, a paste
//   event carrying it (text, HTML, images), and in a text box the text typed in;
// - select all: the browser's own command;
// - undo and redo: in a text box, the browser's own command (an editor has its own keys for both).
//
// A key the page already took (an editor's own Cmd+Z, Cmd+A) is left alone. Off a Mac the browser
// does all of these in the page, so this does nothing there.

export type EditAction = "copy" | "cut" | "paste" | "selectAll" | "undo" | "redo";

type Keys = Pick<KeyboardEvent, "code" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey">;

/** The edit shortcut `e` is on a Mac (Cmd), or null. */
export function editAction(e: Keys): EditAction | null {
  if (!e.metaKey || e.ctrlKey || e.altKey) return null;
  switch (e.code) {
    case "KeyC":
      return e.shiftKey ? null : "copy";
    case "KeyX":
      return e.shiftKey ? null : "cut";
    case "KeyV":
      return e.shiftKey ? null : "paste";
    case "KeyA":
      return e.shiftKey ? null : "selectAll";
    case "KeyZ":
      return e.shiftKey ? "redo" : "undo";
    default:
      return null;
  }
}

/** A text box (input, textarea) the keys type into. */
const isTextBox = (el: Element | null): el is HTMLInputElement | HTMLTextAreaElement =>
  el instanceof HTMLTextAreaElement || (el instanceof HTMLInputElement && !["checkbox", "radio", "button"].includes(el.type));

/** Paste the clipboard into the focused text box or editor. */
async function paste() {
  const target = document.activeElement;
  const editor = target instanceof HTMLElement && target.isContentEditable ? target : null;
  if (!isTextBox(target) && !editor) return;
  if (editor) {
    // Everything the clipboard holds that an editor takes: text, HTML, images
    const data = new DataTransfer();
    try {
      for (const item of await navigator.clipboard.read()) {
        for (const type of item.types) {
          const blob = await item.getType(type);
          if (type === "text/plain" || type === "text/html") data.setData(type, await blob.text());
          else if (type.startsWith("image/")) data.items.add(new File([blob], `pasted.${type.slice(6)}`, { type }));
        }
      }
    } catch {
      const text = await navigator.clipboard.readText().catch(() => "");
      if (text) data.setData("text/plain", text);
    }
    const event = new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true });
    // An editor that takes the paste cancels it; one that does not gets the text typed in
    if (editor.dispatchEvent(event)) document.execCommand("insertText", false, data.getData("text/plain"));
    return;
  }
  const text = await navigator.clipboard.readText().catch(() => "");
  if (text) document.execCommand("insertText", false, text);
}

/** Run the edit shortcuts while `active()` (the page has the keys) and `locked()` (the keyboard
 *  is locked), on a Mac. Returns the detach. */
export function attachEditKeys(o: { active: () => boolean; locked: () => boolean }): () => void {
  const mac = typeof navigator !== "undefined" && /Mac/.test(navigator.platform);
  const onKey = (e: KeyboardEvent) => {
    if (!mac || e.defaultPrevented || e.repeat || !o.active() || !o.locked()) return;
    const action = editAction(e);
    if (!action) return;
    const target = document.activeElement;
    if ((action === "undo" || action === "redo") && !isTextBox(target)) return;
    e.preventDefault();
    if (action === "paste") void paste();
    else document.execCommand(action);
  };
  window.addEventListener("keydown", onKey);
  return () => window.removeEventListener("keydown", onKey);
}
