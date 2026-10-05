// Logs what the first provider request of the process actually carries: the model, the
// reasoning effort, and the speed tier. Read-only — it never changes the payload.
//
// The startup line only says what the wrapper asked pi for. This one is what pi put on the
// wire after mapping `thinkingLevel` through the model's thinkingLevelMap, so it is the line
// to check when confirming a clone runs gpt-6-luna at max on the default tier (no
// `service_tier`, so not the Codex "Fast" tier).

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export function requestLogExtension(pi: ExtensionAPI): void {
  let announced = false;
  pi.on("before_provider_request", (event) => {
    if (announced) return undefined;
    announced = true;
    const payload = event.payload as Record<string, unknown>;
    const effort = (payload.reasoning as { effort?: string } | undefined)?.effort ?? "default";
    const tier = typeof payload.service_tier === "string" ? payload.service_tier : "default";
    console.log(`provider request: model ${String(payload.model)}, effort ${effort}, service_tier ${tier}`);
    return undefined;
  });
}
