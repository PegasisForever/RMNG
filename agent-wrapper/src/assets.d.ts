// Ambient type for Bun text imports (`import x from "./f.md" with { type: "text" }`).
// The wrapper ships as a `bun build --compile` single-exec, so these markdown files are
// inlined into the bundle as string constants at build time (no runtime fs read, which
// would fail against the bunfs virtual filesystem).
declare module "*.md" {
  const content: string;
  export default content;
}
