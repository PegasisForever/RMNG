// Over the clone's screen while the window is in the browser's own fullscreen
// (`./browserFullscreen.ts`): there the keys are not locked, so the dialog asks the operator to
// leave it and use RMNG's Fullscreen button, which locks them. It goes when the browser's
// fullscreen ends; Not now (or Escape) hides it until the next time.
import { Maximize } from "lucide-react";

import { ModalShell } from "~/components/ModalShell";

import { BUTTON, TEXT, TEXT_2 } from "./control/ui";

export function BrowserFullscreenDialog({
  open,
  leaveKeys,
  onExited,
}: {
  /** Still in the browser's fullscreen, and not put off. */
  open: boolean;
  /** The keys that leave the browser's fullscreen here (`leaveBrowserFullscreenKeys`). */
  leaveKeys: string;
  /** Gone (its exit frames ran): Not now, Escape, or the browser's fullscreen ended. */
  onExited: () => void;
}) {
  return (
    <ModalShell open={open} size="sm" onExited={onExited}>
      {(close) => (
        <div className={`flex flex-col gap-3 ${TEXT}`}>
          <h2 className="text-base font-semibold">Use RMNG's fullscreen</h2>
          <p className={`text-sm ${TEXT_2}`}>
            This is the browser's own fullscreen. In it, the browser and the system keep some keys, such as Esc and
            the control panel's keys, so they do not reach the clone.
          </p>
          <p className={`text-sm ${TEXT_2}`}>
            Leave it with <kbd className={`font-medium ${TEXT}`}>{leaveKeys}</kbd>. Then move the mouse to the right
            edge of the window and click Fullscreen{" "}
            <Maximize aria-label="the Fullscreen button" className={`inline size-4 align-text-bottom ${TEXT}`} />.
          </p>
          <div className="flex justify-end">
            <button type="button" autoFocus className={`${BUTTON} px-3 py-1.5 text-sm`} onClick={() => close()}>
              Not now
            </button>
          </div>
        </div>
      )}
    </ModalShell>
  );
}
