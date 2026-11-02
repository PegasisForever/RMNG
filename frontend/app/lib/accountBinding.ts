// What a clone's account is bound to, for one provider: the one reading of the clone's
// `group`, `<provider>Group`, `<provider>Selection` and `<provider>AccountEmail`. The card,
// the Clones tab's row and the Change account dialog all draw from it.
//
// The rule is the server's (`crates/control-server/src/pool.rs`, `rotate_once` and
// `auto_pool_clones`):
// - pool: an `auto` selection on a clone with a pool rotates in that pool. A side with a
//   sticky `<provider>Group` (a clone from before the shared `group`) rotates in it, and so
//   does a retired `group:<name>` selection.
// - auto: an `auto` selection with no pool rotates across every account of the provider.
// - pinned: an email selection keeps that account, also on a clone with a pool. A clone with
//   no selection at all is the oldest shape and is pinned to the account it has.
//
// The server stores no `"none"` selection any more (`normalize_selection` reads it as auto),
// so there is no tokenless state here: a side with no account shows "unassigned".
import type { Clone } from "~/lib/types";

export type Provider = "claude" | "codex";

export interface AccountBinding {
  mode: "pool" | "auto" | "pinned";
  /** The pool the account rotates in. Null unless `mode` is "pool". */
  pool: string | null;
  /** The account installed now. Null when the clone has no token for this provider. */
  email: string | null;
  /** What the dialog's account picker starts on: "auto", or the pinned email. */
  selection: string;
  /** The chip before the account: the pool's name, or "auto". Null for a pin, because the
   *  account beside it already is the pin. */
  badge: string | null;
  /** The text in the account's place: the email, else "unassigned". */
  label: string;
  /** The tooltip, e.g. "Claude: a@x (pinned)". */
  title: string;
}

const NAME: Record<Provider, string> = { claude: "Claude", codex: "Codex" };

function fields(clone: Clone, provider: Provider) {
  return provider === "claude"
    ? { sticky: clone.claudeGroup, selection: clone.claudeSelection, email: clone.claudeAccountEmail }
    : { sticky: clone.codexGroup, selection: clone.codexSelection, email: clone.codexAccountEmail };
}

export function accountBinding(clone: Clone, provider: Provider): AccountBinding {
  const { sticky, selection, email = null } = fields(clone, provider);
  const legacyPool = selection?.startsWith("group:") ? selection.slice("group:".length) : null;
  const pool = legacyPool || (selection === "auto" && clone.group) || sticky || null;
  const mode = pool ? "pool" : selection === "auto" ? "auto" : "pinned";
  const pin = mode === "pinned" ? (selection ?? email) : null;
  const how = mode === "pool" ? `pool ${pool}` : mode;
  return {
    mode,
    pool,
    email,
    selection: pin ?? "auto",
    badge: mode === "pool" ? pool : mode === "auto" ? "auto" : null,
    label: email ?? "unassigned",
    title: email ? `${NAME[provider]}: ${email} (${how})` : `${NAME[provider]}: no account assigned`,
  };
}

/** The clone's pool, which both providers share: what the dialog's pool picker starts on.
 *  A clone from before the shared `group` has it only on a side. */
export function clonePool(clone: Clone): string | null {
  return clone.group ?? accountBinding(clone, "claude").pool ?? accountBinding(clone, "codex").pool;
}
