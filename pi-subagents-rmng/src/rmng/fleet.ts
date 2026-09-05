import { randomUUID } from "node:crypto";
import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { mkdir, writeFile, rename } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import type { RemoteRecord } from "./protocol.ts";

export const packageRoot = fileURLToPath(new URL("../../", import.meta.url));
export const runtimeRoot = join(packageRoot, ".rmng-runtime");

export function controlEnv(): NodeJS.ProcessEnv {
  const env = { ...process.env };
  if (!env.RMNG_CONTROL_URL && existsSync("/etc/environment")) {
    const match = readFileSync("/etc/environment", "utf8").match(/^RMNG_CONTROL_URL=(.+)$/m);
    if (match) env.RMNG_CONTROL_URL = match[1]!.trim().replace(/^["']|["']$/g, "");
  }
  if (!env.RMNG_CONTROL_URL) throw new Error("RMNG_CONTROL_URL is missing. This plugin requires an RMNG clone.");
  return env;
}

export function run(command: string, args: string[], input?: Buffer | string, timeout = 60_000, signal?: AbortSignal): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { env: controlEnv(), stdio: ["pipe", "pipe", "pipe"] });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    let size = 0;
    const timer = setTimeout(() => child.kill("SIGKILL"), timeout);
    const abort = () => child.kill("SIGKILL");
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
    child.stdout.on("data", (data: Buffer) => {
      size += data.length;
      if (size > 16 * 1024 * 1024) child.kill("SIGKILL");
      else stdout.push(data);
    });
    child.stderr.on("data", (data: Buffer) => { if (stderr.length < 200) stderr.push(data); });
    child.stdin.on("error", () => {});
    child.stdin.end(input);
    child.on("error", reject);
    child.on("close", (code) => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
      if (code === 0) resolve(Buffer.concat(stdout).toString());
      else reject(new Error(command + " failed: " + Buffer.concat(stderr).toString().slice(-4000)));
    });
  });
}

export async function rmng(args: string[], input?: Buffer | string, timeout?: number, signal?: AbortSignal): Promise<string> {
  return run("rmng", args, input, timeout, signal);
}

export async function self(): Promise<Record<string, any>> {
  const record = JSON.parse(await rmng(["clone", "self", "--json"]));
  if (!record?.managed || !record.id || !record.source) throw new Error("This process is not inside a managed RMNG clone.");
  return record;
}

export async function save(path: string, value: unknown): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  const temp = path + "." + randomUUID() + ".tmp";
  await writeFile(temp, JSON.stringify(value, null, 2));
  await rename(temp, path);
}

export async function request(record: RemoteRecord, route: string, body?: unknown): Promise<any> {
  if (!record.address) throw new Error("Subclone is not ready: " + record.id);
  const response = await fetch(record.address + route, {
    method: body === undefined ? "GET" : "POST",
    headers: { "content-type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    signal: AbortSignal.timeout(60_000),
  });
  const value = await response.json() as any;
  if (!response.ok) throw new Error(value.error || "Subclone request failed.");
  return value;
}

export async function provision(record: RemoteRecord, parent: Record<string, any>, hostConfig: Record<string, unknown>, signal?: AbortSignal): Promise<void> {
  if (!existsSync(join(runtimeRoot, "bin/node"))) throw new Error("Run scripts/rmng/install.sh before starting a subclone.");
  const seeds = [record.cwd];
  if (!packageRoot.startsWith(record.cwd + "/") && packageRoot !== record.cwd) seeds.push(packageRoot.replace(/\/$/, ""));
  for (const name of ["agents", "skills"]) {
    const source = join(process.env.HOME!, ".pi/agent", name);
    if (existsSync(source)) seeds.push(source);
  }
  const roots = seeds.filter((path, index) => !seeds.some((other, otherIndex) =>
    otherIndex !== index && (path.startsWith(other + "/") || (path === other && otherIndex < index))));
  await rmng(["clone", "create", record.id, "--from", parent.source, "--parent", parent.parent || parent.id,
    "--headless", ...roots.flatMap((path) => ["--seed", path]),
    "--wait", "--timeout", "900", "--json"], undefined, 960_000, signal);
  signal?.throwIfAborted();
  const remoteDir = join(process.env.HOME!, ".pi/rmng-host");
  await rmng(["clone", "exec", record.id, "--", "mkdir", "-p", remoteDir]);
  await rmng(["clone", "exec", record.id, "--", "python3", "-c",
    'import pathlib,sys\npathlib.Path(sys.argv[1]).write_bytes(sys.stdin.buffer.read())',
    join(remoteDir, "config.json")], JSON.stringify({ ...hostConfig, cwd: record.cwd, remoteDir }));
  await rmng(["clone", "exec", "-e", "XDG_RUNTIME_DIR=/run/user/1000", record.id, "--",
    "systemd-run", "--user", "--unit=pi-subagents-host", "--property=Restart=on-failure", "--property=RestartSec=2",
    "--setenv=PATH=" + join(runtimeRoot, "bin") + ":/usr/local/bin:/usr/bin:/bin",
    join(runtimeRoot, "bin/node"), join(packageRoot, "scripts/rmng/host.mjs"), join(remoteDir, "config.json")]);
  const deadline = Date.now() + 120_000;
  let lastError = "The remote host did not start.";
  while (Date.now() < deadline) {
    signal?.throwIfAborted();
    try {
      const ready = JSON.parse(await rmng(["clone", "exec", record.id, "--", "cat", join(remoteDir, "ready.json")]));
      const fleet = JSON.parse(await rmng(["clone", "ls", "--json"]));
      const clone = fleet.clones.find((item: any) => item.id === record.id);
      if (!clone?.localIp) throw new Error("Subclone has no internal address.");
      record.address = "http://" + clone.localIp + ":" + ready.port;
      await request(record, "/health");
      return;
    } catch (error) { lastError = String(error); }
    await delay(2000, undefined, { signal });
  }
  const log = await rmng(["clone", "exec", record.id, "--", "tail", "-c", "4000", join(remoteDir, "host.log")]).catch(() => "");
  throw new Error(lastError + "\n" + log);
}
