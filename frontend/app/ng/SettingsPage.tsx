/** The Settings tab's page, for now: a text box to try the phone's keyboard with. While the
 *  keyboard is up, the whole page stays in view above it (`./systemKeyboard.ts`); the line at
 *  the bottom shows where the page ends. */
export function SettingsPage() {
  return (
    <div className="flex h-full flex-col gap-3 rounded-md border border-neutral-800 p-3">
      <div className="text-sm font-medium text-neutral-300">Settings</div>
      <label className="flex flex-col gap-1.5">
        <span className="text-xs text-neutral-500">Text box (a placeholder)</span>
        <input
          type="text"
          placeholder="Type here"
          autoComplete="off"
          // 16 px or more, or iOS zooms the page in on the box
          className="select-text rounded-md border border-neutral-700 bg-neutral-900 px-3 py-2 text-base text-neutral-100 outline-none placeholder:text-neutral-600 focus:border-neutral-400"
        />
      </label>
      <div className="mt-auto text-center text-xs text-neutral-600">The bottom of the Settings page</div>
    </div>
  );
}
