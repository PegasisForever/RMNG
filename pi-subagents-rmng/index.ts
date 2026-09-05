import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import type {} from "./src/types/pi-runtime-compat.d.ts";
import { withRmngSubclones } from "./src/rmng/extension.ts";

const registerParentExtension = process.env.PI_SUBAGENT_CHILD === "1"
	? undefined
	: (await import("./src/extension/index.ts")).default;

export default function registerSubagentExtension(pi: ExtensionAPI): void {
	if (process.env.PI_SUBAGENT_CHILD === "1") return;
	registerParentExtension?.(process.env.PI_RMNG_REMOTE_HOST === "1" ? pi : withRmngSubclones(pi));
}
