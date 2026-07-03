// The desktop agent's baked-in playbook (operating notes + ticket procedure), embedded at
// BUILD time via a Bun text import so it always ships inside the `bun build --compile`
// single-exec (a runtime read of a bunfs-relative path would ENOENT). This is the FALLBACK:
// at clone creation the control-server injects an editable copy at CONFIG.instructionsPath,
// which wins when present. See agent-wrapper/README.md.
import BAKED_IN_RAW from "../agent-instructions.md" with { type: "text" };
import { readFileSync } from "node:fs";

export const BAKED_IN_INSTRUCTIONS = BAKED_IN_RAW.trim();

/** The system-prompt append: the injected file if present + non-empty, else the baked-in
 *  default. `read` is injectable for testing; defaults to a UTF-8 file read. */
export function resolveSystemAppend(
  injectedPath: string,
  read: (p: string) => string = (p) => readFileSync(p, "utf8"),
): string {
  try {
    const injected = read(injectedPath).trim();
    if (injected) return injected;
  } catch {
    // absent / unreadable (local `bun run` dev, or robustness) — use the baked-in default
  }
  return BAKED_IN_INSTRUCTIONS;
}
