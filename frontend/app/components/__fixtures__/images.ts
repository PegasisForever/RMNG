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

export const images: ImageInfo[] = [
  makeImage({ base: true, inUseBy: [cloneWorking.id, cloneIdle.id] }),
  makeImage({
    id: "sha256:bbbb1111",
    reference: "node20:latest",
    sizeBytes: BigInt(7_200_000_000),
    createdAt: "2026-06-28T09:30:00Z",
    createdFrom: "pegasis0/rmng-template:latest",
  }),
];
