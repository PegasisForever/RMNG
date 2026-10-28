// Who has the clones' layout: the desktop (the active preset) or the phone (`/ng`, the iPhone
// Duo layout). Each device claims it on its own user's actions, never because it sees the
// other device's layout, so the two cannot take turns by themselves (`POST /api/layout/device`).
import { claimLayout } from "./api";

/** A device's claims are at most this far apart: the server changes nothing for a device that
 *  has the layout already, so more would be noise. */
const CLAIM_EVERY_MS = 2000;

/** A function that claims the layout for `device`, at most once per CLAIM_EVERY_MS. The phone
 *  may name its monitor's size (`size`, asked at each claim); without one it keeps its size. */
export function layoutClaimer(
  device: "phone" | "desktop",
  size?: () => { width: number; height: number } | undefined,
): () => void {
  let last = -Infinity;
  return () => {
    const t = performance.now();
    if (t - last < CLAIM_EVERY_MS) return;
    last = t;
    claimLayout(device, size?.()).catch(() => {});
  };
}
