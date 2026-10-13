import { expect, test } from "bun:test";

import { buildForwardCommand, buildSshCommand } from "./ssh";

test("the forward command jumps through the bastion and forwards local to clone port", () => {
  expect(buildForwardCommand("rmng.ts.net", 2222, "pega-abc", 8080, 3000)).toBe(
    "ssh -J rmng@rmng.ts.net:2222 -N -L 8080:localhost:3000 -o StrictHostKeyChecking=accept-new rmng@pega-abc",
  );
});

test("the ssh command jumps through the bastion to the clone", () => {
  expect(buildSshCommand("rmng.ts.net", 2222, "pega-abc")).toBe(
    "ssh -J rmng@rmng.ts.net:2222 -o StrictHostKeyChecking=accept-new rmng@pega-abc",
  );
});
