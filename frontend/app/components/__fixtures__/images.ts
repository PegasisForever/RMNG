// Clone-source images, as `GET /api/images` returns them. Any clone can be committed to one,
// and clone creation picks from these.

import type { ImageInfo } from "~/lib/wire/ImageInfo";

import { cloneIdle, cloneWorking } from "./clones";

export function makeImage(overrides: Partial<ImageInfo> = {}): ImageInfo {
  return {
    id: "sha256:aaaa0000",
    reference: "pegasis0/rmng-template:latest",
    sizeBytes: BigInt(6_800_000_000),
    createdAt: "2026-06-20T12:00:00Z",
    base: false,
    createdFrom: null,
    inUseBy: [],
    ...overrides,
  };
}

/** The instant image ages are measured against, and the one a story hands the picker and the
 *  Images section as their `now`. Against it the two images below read "11d ago" and "3d ago"
 *  rather than whatever today happens to make of them. */
export const imagesNow: number = Date.parse("2026-07-01T12:00:00Z");

/** The clone-source list, freshly built. */
export function makeImages(): ImageInfo[] {
  return [
    makeImage({ base: true, inUseBy: [cloneWorking.id, cloneIdle.id] }),
    makeImage({
      id: "sha256:bbbb1111",
      reference: "node20:latest",
      sizeBytes: BigInt(7_200_000_000),
      createdAt: "2026-06-28T09:30:00Z",
      createdFrom: "pegasis0/rmng-template:latest",
    }),
  ];
}
