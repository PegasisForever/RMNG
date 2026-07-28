import { expect, test } from "bun:test";

import { epochMsToLocalInput, localInputToEpochMs } from "./ChatPanel";

test("datetime-local text round-trips through epoch ms at minute precision", () => {
  // Built from local parts so the assertion holds in any TZ the test runs under.
  const picked = new Date(2026, 6, 27, 15, 30, 0, 0);
  const text = epochMsToLocalInput(picked.getTime());

  expect(text).toBe("2026-07-27T15:30");
  expect(localInputToEpochMs(text)).toBe(picked.getTime());
});

test("zero-pads single-digit months, days, hours, and minutes", () => {
  // An unpadded field ("2026-1-5T9:7") is rejected by the input, silently blanking the picker.
  expect(epochMsToLocalInput(new Date(2026, 0, 5, 9, 7).getTime())).toBe("2026-01-05T09:07");
});

test("an unparseable picker value yields null rather than NaN", () => {
  expect(localInputToEpochMs("")).toBeNull();
  expect(localInputToEpochMs("not-a-date")).toBeNull();
});
