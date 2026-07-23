import { expect, test } from "bun:test";
import { mcpServersFromDescriptor } from "./mcp";

const DESKTOP = { name: "desktop", url: "http://127.0.0.1:9004", alwaysLoad: true };
const LINEAR = { name: "linear", url: "https://mcp.linear.app/mcp", bearerEnv: "LINEAR_API_KEY" };

test("maps desktop with alwaysLoad and no headers", () => {
  const servers = mcpServersFromDescriptor([DESKTOP], {});
  expect(servers.desktop).toEqual({ type: "http", url: "http://127.0.0.1:9004", alwaysLoad: true });
});

test("resolves linear bearer from env", () => {
  const servers = mcpServersFromDescriptor([LINEAR], { LINEAR_API_KEY: "lin_secret" });
  expect(servers.linear).toEqual({
    type: "http",
    url: "https://mcp.linear.app/mcp",
    headers: { Authorization: "Bearer lin_secret" },
  });
});

test("skips a bearer server when its env key is empty", () => {
  expect(mcpServersFromDescriptor([LINEAR], {})).toEqual({});
  expect(mcpServersFromDescriptor([LINEAR], { LINEAR_API_KEY: "" })).toEqual({});
});

test("headless descriptor (desktop already filtered out by the server) yields only linear", () => {
  const servers = mcpServersFromDescriptor([LINEAR], { LINEAR_API_KEY: "k" });
  expect(Object.keys(servers)).toEqual(["linear"]);
});

test("ignores malformed entries", () => {
  const servers = mcpServersFromDescriptor(
    [{ name: "", url: "x" } as never, { url: "y" } as never, DESKTOP],
    {},
  );
  expect(Object.keys(servers)).toEqual(["desktop"]);
});
