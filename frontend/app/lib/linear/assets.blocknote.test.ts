// `~/lib/linear/assets` against the parser that actually reads its output.
//
// `toProxyMarkdown` is a pair of regexes over a string, and the thing on the other side of it
// is BlockNote, whose markdown pipeline is remark plus a ProseMirror DOM parse. A regex that
// looks right and a parser that disagrees is exactly how a link destination ends up rewritten,
// so this file drives the real `BlockNoteEditor` rather than reasoning about what it does.
//
// Separate file because BlockNote needs a DOM: `@happy-dom/global-registrator` puts one on
// the globals before `@blocknote/core` is imported, which is why that import is dynamic.
import { describe, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";

import { toLinearMarkdown, toProxyMarkdown } from "~/lib/linear/assets";

GlobalRegistrator.register();
const { BlockNoteEditor } = await import("@blocknote/core");

const ASSET = "https://uploads.linear.app/ab12/cd34/shot.png";
const PROXIED = "/api/linear/asset?url=https%3A%2F%2Fuploads.linear.app%2Fab12%2Fcd34%2Fshot.png";

/** Every block, flattened, so a nested one counts the same as a top-level one. */
function flatten(blocks: readonly unknown[]): Record<string, unknown>[] {
  const out: Record<string, unknown>[] = [];
  for (const block of blocks as Record<string, unknown>[]) {
    out.push(block);
    out.push(...flatten((block.children ?? []) as unknown[]));
  }
  return out;
}

/** Every `href` in the document, whatever depth it sits at. */
function hrefs(node: unknown): string[] {
  if (Array.isArray(node)) return node.flatMap(hrefs);
  if (node === null || typeof node !== "object") return [];
  const record = node as Record<string, unknown>;
  const here = typeof record.href === "string" ? [record.href] : [];
  return [...here, ...Object.values(record).flatMap(hrefs)];
}

/** What BlockNote makes of one markdown body, and what it writes back out. */
async function through(markdown: string) {
  const editor = BlockNoteEditor.create();
  const blocks = await editor.tryParseMarkdownToBlocks(markdown);
  return { blocks, back: await editor.blocksToMarkdownLossy(blocks) };
}

describe("BlockNote reads the proxy rewrite the way it is meant", () => {
  test("an image block points at the proxy and a link points at Linear", async () => {
    const body = [
      `![screenshot](${ASSET})`,
      "",
      `[click me](${ASSET.replace("shot.png", "evil.html")})`,
      "",
      `Bare in text: ${ASSET} and on it goes.`,
    ].join("\n");

    const { blocks } = await through(toProxyMarkdown(body));
    const images = flatten(blocks).filter((b) => b.type === "image");

    expect(images.map((b) => (b.props as Record<string, unknown>).url)).toEqual([PROXIED]);
    // A link keeps the destination it came with, so it still reads as the Linear URL it is.
    expect(hrefs(blocks)).toEqual([`${ASSET.replace("shot.png", "evil.html")}`]);
    expect(hrefs(blocks).some((h) => h.startsWith("/api/linear/asset"))).toBe(false);
  });

  test("what BlockNote writes back carries no proxy path", async () => {
    // Every image on its own line: BlockNote drops one sitting inside a paragraph, which is
    // its own lossiness and would hide what this test is asking about.
    const body = [
      "# Repro",
      "",
      `![screenshot](${ASSET})`,
      "",
      `[click me](${ASSET.replace("shot.png", "evil.html")})`,
      "",
      "![notes](/uploads/pasted-1.png)",
      "",
      `- ![in a list](${ASSET.replace("shot.png", "listed.png")})`,
    ].join("\n");

    const { back } = await through(toProxyMarkdown(body));
    const stored = toLinearMarkdown(back);

    expect(stored).not.toContain("/api/linear/asset");
    expect(stored).toContain(`![screenshot](${ASSET})`);
    expect(stored).toContain(`[click me](${ASSET.replace("shot.png", "evil.html")})`);
    expect(stored).toContain("![notes](/uploads/pasted-1.png)");
  });

  test("an image the editor itself uploaded round-trips to Linear's own URL", async () => {
    // `MarkdownEditorContainer` hands the editor a proxy path, because that is the only
    // source an `<img>` on this page can load. What Linear stores is the asset URL again.
    const { back } = await through(`![pasted](${PROXIED})`);
    expect(toLinearMarkdown(back).trim()).toBe(`![pasted](${ASSET})`);
  });
});
