// After a sign-in, whether the account list on this page shows it working yet. The server
// stores the account at once, but its usage comes from a poll a moment later, over the state
// stream; a replaced account leaves the list in the same way. The sign-in dialog waits for both
// before it closes, so what the operator sees next is the account as it now is.
import type { ClaudeUsage } from "~/lib/types";

export type SignInCheck = {
  /** The new account is in the list with fresh numbers, no error, and a token that works. */
  works: boolean;
  /** The account it replaced is out of the list (true when it replaced none). */
  oldGone: boolean;
  /** What the list says is wrong with the new account, while it does not work. */
  problem: string | null;
};

const same = (a: string, b: string) => a.trim().toLowerCase() === b.trim().toLowerCase();

/** `email` signed in for `provider`, standing in for `replaced` (null: none, or itself). */
export function checkSignIn(
  accounts: ClaudeUsage[],
  provider: "claude" | "codex",
  email: string,
  replaced: string | null,
): SignInCheck {
  const mine = accounts.filter((a) => (a.provider ?? "claude") === provider);
  const rows = mine.filter((a) => same(a.email, email));
  const works =
    rows.length > 0 &&
    rows.every((a) => a.assignable !== false && !a.error && !a.stale && !!(a.fiveHour || a.sevenDay || a.fable));
  const oldGone = !replaced || same(replaced, email) || !mine.some((a) => same(a.email, replaced));
  const problem = works ? null : (rows.find((a) => a.error)?.error ?? null);
  return { works, oldGone, problem };
}
