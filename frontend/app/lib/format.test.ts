// `sizeBytes` is typed bigint but does not always arrive as one, so these pin every
// representation `formatBytes` is handed at runtime against the same byte count.
import { expect, test } from "bun:test";

import { formatBytes } from "./format";

test("the same byte count reads the same as a bigint, a number, or a string", () => {
  expect(formatBytes(7_200_000_000n)).toBe("6.7 GB");
  expect(formatBytes(7_200_000_000)).toBe("6.7 GB");
  // Storybook's Controls panel returns an edited arg as a string. A reader that branched on
  // `typeof x === "bigint"` would skip the coercion here and collapse the size to "0 B".
  expect(formatBytes("7200000000" as unknown as number)).toBe("6.7 GB");
});

test("the unit steps up with the magnitude", () => {
  expect(formatBytes(512)).toBe("512 B");
  expect(formatBytes(1024)).toBe("1.0 KB");
  expect(formatBytes(1_048_576)).toBe("1.0 MB");
  expect(formatBytes(20_078_170_112n)).toBe("18.7 GB");
});

test("a value that is not a size reads as zero rather than as garbage", () => {
  expect(formatBytes(0)).toBe("0 B");
  expect(formatBytes(-1)).toBe("0 B");
  expect(formatBytes(Number.NaN)).toBe("0 B");
  expect(formatBytes("" as unknown as number)).toBe("0 B");
  expect(formatBytes("not a number" as unknown as number)).toBe("0 B");
});
