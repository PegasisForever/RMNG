// Guards the exit-animation contract `ModalShell` owns, which a browser-only symptom once
// broke silently: flipping `animation-direction` on an already-finished same-name animation
// does NOT restart it, so the dialog sat frozen until the unmount timer fired and no exit
// ever played. The `-in` → `-out` animation-name change is the restart; these tests fail if
// anyone merges the two names back together.
//
// NOT covered here, because no renderer is installed (there is no @testing-library and no
// .test.tsx in this project): that `ModalShell` actually swaps to the exit class on a close,
// that `onExited` fires after `MODAL_EXIT_MS` and not before, and that Escape reaches only
// the topmost dialog. What is reachable without a renderer is the derivation those three
// rest on — the class each state picks, and the delay against the frames it has to cover.
// The stacking rule itself is tested through `ownsEscape` in useModalEscape's own tests.
import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import {
  MODAL_ANIMATION,
  MODAL_ANIMATION_MS,
  backdropClass,
  panelClass,
  type ModalSize,
} from "./ModalShell";
import { MODAL_EXIT_MS } from "~/lib/useModalExit";

const SIZES: ModalSize[] = ["sm", "md", "lg", "panel"];

test("exit uses its own animation name so the animation restarts on close", () => {
  expect(MODAL_ANIMATION.panel.exit).not.toBe(MODAL_ANIMATION.panel.enter);
  expect(MODAL_ANIMATION.backdrop.exit).not.toBe(
    MODAL_ANIMATION.backdrop.enter,
  );
});

test("closing swaps the entry class for the exit one, at every size", () => {
  for (const size of SIZES) {
    expect(panelClass(size, false)).toContain(MODAL_ANIMATION.panel.enter);
    expect(panelClass(size, false)).not.toContain(MODAL_ANIMATION.panel.exit);
    expect(panelClass(size, true)).toContain(MODAL_ANIMATION.panel.exit);
    expect(panelClass(size, true)).not.toContain(MODAL_ANIMATION.panel.enter);
  }
  expect(backdropClass("base", false)).toContain(
    MODAL_ANIMATION.backdrop.enter,
  );
  expect(backdropClass("base", true)).toContain(MODAL_ANIMATION.backdrop.exit);
});

test("the close swaps only the animation, never the panel's shape", () => {
  // The exit is one class change. A size that also moved or resized on the way out would
  // fight its own keyframes, which scale the panel from its resting box.
  for (const size of SIZES) {
    const shape = (closing: boolean) =>
      panelClass(size, closing)
        .split(" ")
        .filter(
          (c) =>
            c !== MODAL_ANIMATION.panel.enter &&
            c !== MODAL_ANIMATION.panel.exit,
        );
    expect(shape(true)).toEqual(shape(false));
  }
});

test("the over rung sits above the base one", () => {
  // The sign-in dialog opens on top of the settings panel. The Escape stack already hands it
  // the key; the z rungs have to agree, or the operator types into what looks like the panel.
  expect(backdropClass("base", false)).toContain("z-50");
  expect(backdropClass("over", false)).toContain("z-[60]");
});

test("the exit delay covers the frames it waits for", () => {
  expect(MODAL_EXIT_MS).toBeGreaterThan(MODAL_ANIMATION_MS);
});

// The four class names above are only names until `app.css` defines them, and two of the
// rules they point at were each written for a symptom that reached a browser: the separate
// `-out` keyframes (see the header) and the `forwards` fill that holds the panel at its end
// state, without which the dialog snapped back to full visibility for the frames between the
// animation finishing and the unmount timer firing. Nothing else machine-checks the
// stylesheet now that the frame has an owner, so the seam between the two is checked here.
const css = readFileSync(join(import.meta.dir, "..", "app.css"), "utf8");

function ruleBody(cls: string): string {
  const rule = new RegExp(`\\.${cls}\\s*\\{([^}]*)\\}`, "s").exec(css);
  if (!rule) throw new Error(`.${cls} rule not found in app.css`);
  return rule[1];
}

test("every class the shell emits is defined in app.css", () => {
  for (const pair of [MODAL_ANIMATION.backdrop, MODAL_ANIMATION.panel]) {
    for (const cls of [pair.enter, pair.exit]) {
      // The rule and the keyframes share the name, which is the whole point of the
      // `-in`/`-out` split: the class IS the animation the browser restarts on.
      expect(ruleBody(cls)).toContain("animation:");
      expect(css).toContain(`@keyframes ${cls}`);
    }
  }
});

test("the exit rules hold their end state and run for the declared time", () => {
  for (const cls of [MODAL_ANIMATION.backdrop.exit, MODAL_ANIMATION.panel.exit]) {
    expect(ruleBody(cls)).toContain("forwards");
  }
  for (const pair of [MODAL_ANIMATION.backdrop, MODAL_ANIMATION.panel]) {
    for (const cls of [pair.enter, pair.exit]) {
      expect(ruleBody(cls)).toContain(`${MODAL_ANIMATION_MS}ms`);
    }
  }
});
