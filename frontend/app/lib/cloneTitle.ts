// What a clone is called on screen: the operator's own name when they renamed it, else its
// ticket's live title, else the name it was made with, else its hostname (`id`).
import type { Clone } from "~/lib/types";

export function cloneTitle(clone: Clone, ticket?: { title: string } | null): string {
  if (clone.named && clone.displayName) return clone.displayName;
  return ticket?.title ?? clone.displayName ?? clone.id;
}
