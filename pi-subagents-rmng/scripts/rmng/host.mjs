import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { appendFileSync, readFileSync } from "node:fs";
const root = fileURLToPath(new URL("../../", import.meta.url));
const config = JSON.parse(readFileSync(process.argv[2], "utf8"));
const log = (value) => appendFileSync(join(config.remoteDir, "host.log"), String(value) + "\n");
console.error = (...args) => log(args.join(" "));
console.log = console.error;
process.on("uncaughtException", (error) => { log(error.stack || error); process.exit(1); });
process.on("unhandledRejection", (error) => { log(error?.stack || error); process.exit(1); });
process.on("exit", (code) => log("Host exited with code " + code));
try {
  const piRoot = join(root, ".rmng-runtime/node_modules/@earendil-works/pi-coding-agent");
  process.env.PI_SUBAGENTS_PI_CODING_AGENT_PACKAGE_ROOT = piRoot;
  process.env.PI_SUBAGENTS_TEMP_ROOT = join(config.remoteDir, "runs");
  process.env.PI_RMNG_REMOTE_HOST = "1";
  const { createJiti } = await import(join(root, "node_modules/jiti/lib/jiti.mjs"));
  log("Resolving pi runtime modules.");
  const jiti = createJiti(import.meta.url);
  const { resolveHostPeerAliases } = await jiti.import(join(root, "src/runs/background/runner-aliases.ts"));
  const { aliases: alias, missing } = resolveHostPeerAliases(piRoot);
  if (missing.length) throw new Error("Missing pi runtime modules: " + missing.join(", "));
  process.env.JITI_ALIAS = JSON.stringify(alias);
  const runtime = createJiti(import.meta.url, { alias });
  log("Loading the upstream plugin.");
  const { startHost } = await runtime.import(join(root, "src/rmng/host.ts"));
  log("Starting the remote host.");
  await startHost(config);
} catch (error) {
  log(error.stack || error);
  process.exit(1);
}
