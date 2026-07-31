import type { ReactNode } from "react";

/** A phone-sized box for the mobile stories: 390x844, the logical size of a recent iPhone.
 *
 *  The mobile pages size to their container rather than to the viewport, which is what lets
 *  this frame work at all — the real route wraps them in `100dvh` instead. The rounded
 *  bezel is only there so a story reads as a phone at a glance in the Storybook canvas. */
export function PhoneFrame({ children }: { children: ReactNode }) {
  return (
    <div className="h-[844px] w-[390px] overflow-hidden rounded-[2.25rem] border-[10px] border-slate-800 bg-white shadow-2xl dark:border-slate-700 dark:bg-slate-950">
      {children}
    </div>
  );
}
