import { describe, expect, test } from "bun:test";

import {
  ASSET_PROXY,
  assetProxyUrl,
  isLinearAsset,
  linearAssetUrl,
  toLinearMarkdown,
  toProxyMarkdown,
} from "~/lib/linear/assets";

const ASSET = "https://uploads.linear.app/ab12/cd34/shot.png";
const PROXIED = "/api/linear/asset?url=https%3A%2F%2Fuploads.linear.app%2Fab12%2Fcd34%2Fshot.png";

describe("isLinearAsset", () => {
  test("takes the asset host over https and nothing else", () => {
    expect(isLinearAsset(ASSET)).toBe(true);
    // Userinfo does not move the host, and the parser folds case for us.
    expect(isLinearAsset("https://evil.tld@uploads.linear.app/x")).toBe(true);
    expect(isLinearAsset("https://UPLOADS.LINEAR.APP/x")).toBe(true);

    for (const url of [
      "https://uploads.linear.app.example.net/x",
      "https://evil.example/uploads.linear.app/x",
      "http://uploads.linear.app/x",
      "https://storage.googleapis.com/x",
      "/uploads/shot.png",
      "not a url",
      "",
    ]) {
      expect(isLinearAsset(url)).toBe(false);
    }
  });
});

describe("assetProxyUrl", () => {
  test("points a Linear asset at the same-origin route", () => {
    expect(assetProxyUrl(ASSET)).toBe(PROXIED);
  });

  test("leaves every other image where it is", () => {
    // The clone dialog's own store, which this server already serves.
    expect(assetProxyUrl("/uploads/shot.png")).toBe("/uploads/shot.png");
    expect(assetProxyUrl("https://example.com/cat.png")).toBe("https://example.com/cat.png");
  });
});

describe("linearAssetUrl", () => {
  test("is the inverse of assetProxyUrl", () => {
    expect(linearAssetUrl(PROXIED)).toBe(ASSET);
    expect(linearAssetUrl(assetProxyUrl(ASSET))).toBe(ASSET);
  });

  test("leaves anything that is not a proxy path alone", () => {
    expect(linearAssetUrl(ASSET)).toBe(ASSET);
    expect(linearAssetUrl("/uploads/shot.png")).toBe("/uploads/shot.png");
  });

  test("reverses every proxy path, whatever it points at", () => {
    // Nothing shaped like a proxy path may reach Linear, so the reverse never declines one.
    const other = "/api/linear/asset?url=https%3A%2F%2Fexample.com%2Fx";
    expect(linearAssetUrl(other)).toBe("https://example.com/x");
  });
});

describe("markdown round trip", () => {
  const body = [
    "# Repro",
    "",
    `![screenshot](${ASSET})`,
    "",
    `Second one: ![again](${ASSET.replace("shot.png", "other.png")}) inline.`,
    "",
    "Local image stays local: ![notes](/uploads/pasted-1.png)",
    "",
    "A link that is not an image: [Linear](https://linear.app/team/issue/WE-1)",
  ].join("\n");

  test("every Linear asset goes behind the proxy and nothing else moves", () => {
    const proxied = toProxyMarkdown(body);
    expect(proxied).toContain(`![screenshot](${PROXIED})`);
    expect(proxied).toContain("![notes](/uploads/pasted-1.png)");
    expect(proxied).toContain("[Linear](https://linear.app/team/issue/WE-1)");
    expect(proxied).not.toContain("(https://uploads.linear.app/");
  });

  test("what Linear stores is byte-identical to what it sent", () => {
    expect(toLinearMarkdown(toProxyMarkdown(body))).toBe(body);
  });

  test("both directions are idempotent", () => {
    const proxied = toProxyMarkdown(body);
    expect(toProxyMarkdown(proxied)).toBe(proxied);
    expect(toLinearMarkdown(body)).toBe(body);
  });

  test("an empty body survives both ways", () => {
    expect(toProxyMarkdown("")).toBe("");
    expect(toLinearMarkdown("")).toBe("");
  });

  test("an HTML img tag is rewritten too, quotes intact", () => {
    const html = `<img src="${ASSET}" alt="shot">`;
    expect(toProxyMarkdown(html)).toBe(`<img src="${PROXIED}" alt="shot">`);
    expect(toLinearMarkdown(toProxyMarkdown(html))).toBe(html);
  });
});

// An `<img>` source has to be same-origin and nothing else in a body does, so an image
// destination is the only thing pointed at the proxy. A link and a bare URL stay readable
// and already work in a browser.
describe("only an image destination crosses to the proxy", () => {
  test("a link to the asset host keeps pointing at Linear", () => {
    for (const md of [
      `[click me](${ASSET})`,
      `[click me](${ASSET} "with a title")`,
      `[![thumb](${ASSET})](${ASSET.replace("shot.png", "full.png")})`,
      `<${ASSET}>`,
      `Bare in text: ${ASSET} and on it goes.`,
      `[ref]: ${ASSET}`,
      `<a href="${ASSET}">click me</a>`,
    ]) {
      // The image inside the linked-thumbnail case is the one thing that may move.
      const expected = md.replace(`![thumb](${ASSET})`, `![thumb](${PROXIED})`);
      expect(toProxyMarkdown(md)).toBe(expected);
    }
  });

  test("an image destination still moves, in every shape a body carries one", () => {
    expect(toProxyMarkdown(`![alt](${ASSET})`)).toBe(`![alt](${PROXIED})`);
    expect(toProxyMarkdown(`![](${ASSET})`)).toBe(`![](${PROXIED})`);
    expect(toProxyMarkdown(`![alt](<${ASSET}>)`)).toBe(`![alt](<${PROXIED}>)`);
    expect(toProxyMarkdown(`![alt](${ASSET} "title")`)).toBe(`![alt](${PROXIED} "title")`);
    expect(toProxyMarkdown(`<img src='${ASSET}'>`)).toBe(`<img src='${PROXIED}'>`);
    expect(toProxyMarkdown(`<IMG ALT="x" SRC=${ASSET}>`)).toBe(`<IMG ALT="x" SRC=${PROXIED}>`);
  });

  test("an unclosed image opener cannot reach a link on the next line", () => {
    const md = `![unclosed\n\n[click me](${ASSET})`;
    expect(toProxyMarkdown(md)).toBe(md);
  });

  test("what the editor holds still reverses wherever a proxy path sits", () => {
    // The reverse stays wide: nothing shaped like a proxy path may reach Linear, whether the
    // editor put it in an image destination or an operator typed it into a link.
    const typed = `[typed](${PROXIED}) and ![real](${PROXIED})`;
    expect(toLinearMarkdown(typed)).toBe(`[typed](${ASSET}) and ![real](${ASSET})`);
    expect(toLinearMarkdown(typed)).not.toContain("/api/linear/asset");
  });
});

// `encodeURIComponent` leaves `!`, `'`, `(`, `)`, `*`, and `~` literal, and `PROXY_URL` stops
// a path at `'` or `)`. Unreachable through `uploadToLinear`, which returns a `/uuid/uuid/uuid`
// asset URL, and closed anyway: the failure mode is a proxy path stored in a Linear body,
// which is a broken image for everyone in the workspace.
describe("the encoder and the pattern that reads it back agree", () => {
  test("a path minted from an awkward URL still reverses", () => {
    for (const url of [
      "https://x'y@uploads.linear.app/a.png",
      "https://uploads.linear.app/a'b.png",
      "https://uploads.linear.app/a(b).png",
      "https://uploads.linear.app/a!b~c*d.png",
      "https://uploads.linear.app/a b.png",
      "https://uploads.linear.app/a]b\"c.png",
    ]) {
      const proxied = assetProxyUrl(url);
      expect(proxied).not.toBe(url);
      expect(linearAssetUrl(proxied)).toBe(url);
      // And the same path survives a round trip inside an image destination.
      const md = `![alt](${proxied})`;
      expect(toLinearMarkdown(md)).toBe(`![alt](${url})`);
      expect(toLinearMarkdown(md)).not.toContain("/api/linear/asset");
    }
  });

  test("every character a proxy path can hold is one the pattern matches", () => {
    const minted = assetProxyUrl("https://uploads.linear.app/!'()*~ \"]<>&?#.png");
    expect(minted.slice(`${ASSET_PROXY}?url=`.length)).toMatch(/^[A-Za-z0-9\-_.%]+$/);
  });
});
