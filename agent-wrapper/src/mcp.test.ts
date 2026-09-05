import { expect, test } from "bun:test";
import { mcpConfigFromDescriptor } from "./mcp";

const DESKTOP = { name: "desktop", url: "http://127.0.0.1:9004", alwaysLoad: true };
const LINEAR = { name: "linear", url: "https://mcp.linear.app/mcp", bearerEnv: "LINEAR_API_KEY" };

test("maps desktop to eager direct tools with no headers", () => {
  const { mcpServers } = mcpConfigFromDescriptor([DESKTOP], {});
  expect(mcpServers.desktop).toEqual({
    url: "http://127.0.0.1:9004",
    lifecycle: "eager",
    directTools: true,
  });
});

test("resolves linear bearer from env", () => {
  const { mcpServers } = mcpConfigFromDescriptor([LINEAR], { LINEAR_API_KEY: "lin_secret" });
  expect(mcpServers.linear).toEqual({
    url: "https://mcp.linear.app/mcp",
    headers: { Authorization: "Bearer lin_secret" },
  });
});

test("skips a bearer server when its env key is empty", () => {
  expect(mcpConfigFromDescriptor([LINEAR], {}).mcpServers).toEqual({});
  expect(mcpConfigFromDescriptor([LINEAR], { LINEAR_API_KEY: "" }).mcpServers).toEqual({});
});

test("headless descriptor (desktop already filtered out by the server) yields only linear", () => {
  const { mcpServers } = mcpConfigFromDescriptor([LINEAR], { LINEAR_API_KEY: "k" });
  expect(Object.keys(mcpServers)).toEqual(["linear"]);
});

test("ignores malformed entries", () => {
  const { mcpServers } = mcpConfigFromDescriptor(
    [{ name: "", url: "x" } as never, { url: "y" } as never, DESKTOP],
    {},
  );
  expect(Object.keys(mcpServers)).toEqual(["desktop"]);
});
