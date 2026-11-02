// `focusPage`: the browser window comes to the front with the keys in the page. In the shell's
// frame (`public/shell.html`) a focus on the shell's window alone leaves the keys with the shell,
// so this page's own window must be focused after it. Needs a DOM: `@happy-dom/global-registrator`.
import { afterEach, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";

if (!GlobalRegistrator.isRegistered) GlobalRegistrator.register();
const { focusPage } = await import("./host");

const ownParent = Object.getOwnPropertyDescriptor(window, "parent");
const ownFocus = window.focus;
afterEach(() => {
  if (ownParent) Object.defineProperty(window, "parent", ownParent);
  else delete (window as { parent?: unknown }).parent;
  window.focus = ownFocus;
});

test("in the shell's frame: the shell's window, then this page's", () => {
  const order: string[] = [];
  const shell = { rmngShell: { version: 1 }, focus: () => order.push("shell") };
  Object.defineProperty(window, "parent", { configurable: true, get: () => shell });
  window.focus = () => order.push("page");
  focusPage();
  expect(order).toEqual(["shell", "page"]);
});

test("in a window of its own: this page's window", () => {
  const order: string[] = [];
  window.focus = () => order.push("page");
  focusPage();
  expect(order).toEqual(["page"]);
});
