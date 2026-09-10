import { expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

import { OperationProgress } from "./OperationProgress";
import { makeOperation } from "./__fixtures__/operations";
import { GLASS_OUTLINE } from "~/lib/glass";

function render(overrides: Parameters<typeof makeOperation>[0] = {}) {
  return renderToStaticMarkup(
    createElement(OperationProgress, { op: makeOperation(overrides) }),
  );
}

test("job cards use the board-card frame and separate target from source details", () => {
  const html = render({ source: "registry.example.com/team/template:latest" });
  expect(html).toContain("group/card");
  expect(html).toContain(GLASS_OUTLINE);
  expect(html).toContain("rounded-lg");
  expect(html).toMatch(/<h3[^>]*>pega-per-9<\/h3>/);
  expect(html).toContain("From registry.example.com/team/template:latest");
});

test.each([
  [0, 0],
  [45, 45],
  [100, 100],
  [-10, 0],
  [150, 100],
  [NaN, 0],
])(
  "progress %s renders an accessible, bounded value of %s",
  (input, expected) => {
    const html = render({ pct: input });
    expect(html).toContain('role="progressbar"');
    expect(html).toContain(`aria-valuenow="${expected}"`);
    expect(html).toContain(`style="width:${expected}%"`);
  },
);

test.each([
  ["running", "Running", "bg-sky-500"],
  ["done", "Complete", "bg-emerald-500"],
  ["error", "Failed", "bg-red-500"],
] as const)(
  "%s jobs retain status text and progress color",
  (status, label, color) => {
    const html = render({ status });
    expect(html).toContain(`${label}: Provisioning container`);
    expect(html).toContain(color);
  },
);

test("the log starts collapsed and its button points to selectable output", () => {
  const html = render({ log: [] });
  expect(html).toContain('aria-expanded="false"');
  const controlledId = html.match(/aria-controls="([^"]+)"/)?.[1];
  expect(controlledId).toBeDefined();
  expect(html).toContain(`<pre id="${controlledId}" hidden=""`);
  expect(html).toContain("select-text");
  expect(html).toContain("(no output yet)");
});
