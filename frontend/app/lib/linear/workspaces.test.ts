import { describe, expect, it } from "bun:test";

import type { LinearWorkspace } from "./types";
import { mergeWorkspaces, workspaceFromResponse, workspaceHomeUrl } from "./workspaces";

/** The shape api.linear.app really answers `organization { id name urlKey }` with. */
const REAL = {
  data: {
    organization: {
      id: "d41ca53e-9014-4d04-b567-5eb5a10a5ac9",
      name: "Personal",
      urlKey: "pegasis",
    },
  },
};

describe("workspaceFromResponse", () => {
  it("reads the workspace out of a real answer", () => {
    expect(workspaceFromResponse(REAL.data)).toEqual({
      id: "d41ca53e-9014-4d04-b567-5eb5a10a5ac9",
      name: "Personal",
      urlKey: "pegasis",
    });
  });

  it("falls back to the slug when the workspace has no name", () => {
    const found = workspaceFromResponse({ organization: { id: "o1", name: "", urlKey: "acme" } });
    expect(found?.name).toBe("acme");
  });

  it("refuses an answer with no id or no slug, rather than drawing a dead link", () => {
    expect(workspaceFromResponse({ organization: { id: "o1", urlKey: "" } })).toBeNull();
    expect(workspaceFromResponse({ organization: { urlKey: "acme" } })).toBeNull();
    expect(workspaceFromResponse({ organization: null })).toBeNull();
    expect(workspaceFromResponse({})).toBeNull();
    expect(workspaceFromResponse(null)).toBeNull();
  });
});

describe("workspaceHomeUrl", () => {
  it("is the same prefix every issue URL carries", () => {
    const workspace: LinearWorkspace = { id: "o1", name: "Personal", urlKey: "pegasis" };
    expect(workspaceHomeUrl(workspace)).toBe("https://linear.app/pegasis");
  });

  it("is empty for a blank slug, so nothing opens linear.app itself", () => {
    expect(workspaceHomeUrl({ id: "o1", name: "Personal", urlKey: "  " })).toBe("");
  });
});

describe("mergeWorkspaces", () => {
  const personal: LinearWorkspace = { id: "o1", name: "Personal", urlKey: "pegasis" };
  const webapp: LinearWorkspace = { id: "o2", name: "Webapp", urlKey: "webapp-co" };

  it("keeps config order and drops the keys that answered nothing", () => {
    expect(mergeWorkspaces([personal, null, webapp])).toEqual([personal, webapp]);
  });

  it("collapses two keys for one workspace onto its id, not its name", () => {
    // A second personal key, renamed in between. Same organization, so one row.
    const renamed = { ...personal, name: "Personal (old)" };
    expect(mergeWorkspaces([personal, renamed])).toEqual([personal]);
  });

  it("is empty when no key answered", () => {
    expect(mergeWorkspaces([null, null])).toEqual([]);
  });
});
