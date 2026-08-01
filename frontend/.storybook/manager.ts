// Manager-side setup. Storybook bundles this file into the top window, the realm that
// draws the sidebar, the toolbar and the addon panels. The preview iframe is a separate
// realm with its own intrinsics, so `preview.ts` cannot reach anything here and this file
// cannot reach a story.

// ts-rs maps Rust `u64` to TypeScript `bigint`, so the wire types in `app/lib/wire/` carry
// bigint fields (`LxcStats.memUsed`, `CloneTokens.inputTokens`, `ScheduledMessage.at`, and
// a dozen more) and the fixtures build real BigInt values for them. Args travel across the
// channel into this realm unchanged. The Controls panel then renders an object arg by
// running `JSON.stringify` over it, JSON has no bigint, and the call throws
// `TypeError: Do not know how to serialize a BigInt`. The panel catches that in its error
// boundary and every control on the story disappears, including the ones with no bigint in
// them.
//
// `JSON.stringify` looks up `toJSON` on a BigInt exactly as it does on an object: ECMA-262
// SerializeJSONProperty names BigInt alongside Object in the step that fetches the method.
// So giving the prototype one keeps every control on every story.
//
// Two costs. A bigint reads as a quoted string in the raw JSON view, `"20078170112"`
// rather than a bare number, which is the closest JSON can get without losing digits above
// 2^53. Edit that JSON and blur the field and the value comes back a string rather than a
// bigint. That is safe only for a reader that coerces what it got: `formatBytes` and
// `formatTokenCount` both call `Number(...)` first, so an edited value still renders. A
// reader that branches on `typeof x === "bigint"` instead passes the string straight
// through and draws it wrong, so keep new readers of these fields on `Number(...)`.
//
// Patching a builtin is contained here because `.storybook/manager.ts` is an entry point
// for Storybook's manager builder and nothing else. `react-router build` walks the app
// from `app/root.tsx` and never resolves a path under `.storybook/`.
(BigInt.prototype as unknown as { toJSON: () => string }).toJSON = function toJSON(
  this: bigint,
): string {
  return this.toString();
};
