// Which team a reopened dialog starts on. The rule is one line, and the line that matters is
// the existence check: a remembered key nothing claims any more must not survive.
import { expect, test } from "bun:test";

import { preferredTicketTeam } from "./lastTicketTeam";
import type { PresetRedacted } from "./wire/PresetRedacted";

function team(key: string) {
  return { key, preset: { name: key, labels: [key] } as PresetRedacted };
}

test("the remembered team wins over the first one", () => {
  expect(preferredTicketTeam([team("we"), team("dev")], "dev")).toBe("dev");
});

test("nothing remembered falls back to the first team", () => {
  expect(preferredTicketTeam([team("we"), team("dev")], null)).toBe("we");
});

// A preset can lose a label between tickets. Starting on a team no preset claims would leave
// the dropdown showing something the create then refuses.
test("a remembered team nobody claims falls back to the first", () => {
  expect(preferredTicketTeam([team("we")], "ops")).toBe("we");
});

// `teamKeysOf` lowercases every key, and storage holds whatever was written last.
test("the comparison ignores case and surrounding space", () => {
  expect(preferredTicketTeam([team("we"), team("dev")], " DEV ")).toBe("dev");
});

test("no teams at all is blank rather than undefined", () => {
  expect(preferredTicketTeam([], "we")).toBe("");
});
