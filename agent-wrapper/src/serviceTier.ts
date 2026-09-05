// Forces the Codex "Fast" speed tier on every provider request.
//
// The Codex catalog exposes gpt-5.6-luna with one service tier, `priority`, displayed as
// "Fast" (1.5x speed, increased usage). pi-ai accepts `service_tier` in the request body but
// does not plumb it down from the session, so this extension sets it on the payload instead.
//
// Reasoning effort travels the supported route: `thinkingLevel` on the session maps through
// the model's thinkingLevelMap to `reasoning.effort`. Only the speed tier needs this hook.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

/** OpenAI's id for the Fast tier. */
const FAST_SERVICE_TIER = "priority";

export function serviceTierExtension(pi: ExtensionAPI): void {
  let announced = false;
  pi.on("before_provider_request", (event) => {
    const payload = event.payload as Record<string, unknown>;
    if (payload.service_tier === FAST_SERVICE_TIER) return undefined;
    if (!announced) {
      announced = true;
      const effort = (payload.reasoning as { effort?: string } | undefined)?.effort ?? "default";
      console.log(`provider request: model ${String(payload.model)}, effort ${effort}, service_tier ${FAST_SERVICE_TIER}`);
    }
    return { ...payload, service_tier: FAST_SERVICE_TIER };
  });
}
