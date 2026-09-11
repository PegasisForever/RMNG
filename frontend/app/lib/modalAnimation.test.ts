// Guards the modal exit-animation contract in `../app.css`, which a browser-only
// symptom once broke silently: flipping `animation-direction` on an already-finished
// same-name animation does NOT restart it, so the dialog sat frozen until the unmount
// timer fired and no exit ever played. The `-in` → `-out` animation-name change is the
// restart; these tests fail if anyone merges the two back together.
import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { MODAL_EXIT_MS } from "./useModalExit";

const css = readFileSync(join(import.meta.dir, "..", "app.css"), "utf8");

function ruleBody(cls: string): string {
  const rule = new RegExp(`\\.${cls}\\s*\\{([^}]*)\\}`, "s").exec(css);
  if (!rule) throw new Error(`.${cls} rule not found in app.css`);
  return rule[1];
}

function animationName(cls: string): string {
  const anim = /animation:\s*([A-Za-z0-9_-]+)/.exec(ruleBody(cls));
  if (!anim) throw new Error(`.${cls} sets no animation in app.css`);
  return anim[1];
}

function animationMs(cls: string): number {
  const ms = /animation:[^;]*?(\d+)ms/.exec(ruleBody(cls));
  if (!ms) throw new Error(`.${cls} sets no duration in app.css`);
  return Number(ms[1]);
}

test("modal exit uses its own keyframes so the animation restarts on close", () => {
  expect(animationName("rmng-modal-out")).not.toBe(
    animationName("rmng-modal-in"),
  );
  expect(animationName("rmng-backdrop-out")).not.toBe(
    animationName("rmng-backdrop-in"),
  );
});

test("the exit delay covers the exit animation", () => {
  expect(animationMs("rmng-modal-out")).toBeLessThan(MODAL_EXIT_MS);
  expect(animationMs("rmng-backdrop-out")).toBeLessThan(MODAL_EXIT_MS);
});

test("exit holds its end state until unmount (no full-visibility flash)", () => {
  // The unmount timer fires after the animation ends; without a forwards fill both
  // elements snap back to natural full visibility for those frames — one flash, then
  // gone. This fails if the fill is dropped from either exit rule.
  for (const cls of ["rmng-modal-out", "rmng-backdrop-out"]) {
    expect(ruleBody(cls)).toMatch(/animation:[^;]*\b(forwards|both)\b/);
  }
});
