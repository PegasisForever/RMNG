// Add a Claude or Codex account by signing in to the provider from this page.
//
// Two server calls, one per step. `begin` mints a PKCE verifier and hands back the URL to
// open; `complete` takes whatever the browser landed on, redeems the code, stores the
// account and joins it to the chosen pool. Nothing below knows there is a server at all.
//
// The dialog does not close when the server answers: the server has stored the account then,
// but its numbers come from a poll a moment later, and a replaced account leaves the list in
// the same way. It shows those steps, and closes when the account list on this page
// (`accounts`, the live state) shows the account working and the old one gone
// (`~/lib/signInCheck.ts`). Past SLOW_MS it says it is late, and can be closed.
//
// Switching provider starts a fresh sign-in, because the two are separate logins with
// separate verifiers. The one already waiting is simply abandoned: it expires on its own,
// and it names no account, so nothing is left behind by walking away from it.
//
// The markup is ImportAccountModalView.
import { useEffect, useState } from "react";

import { ImportAccountModalView } from "~/components/ImportAccountModalView";
import { beginLogin, completeLogin } from "~/lib/api";
import { checkSignIn } from "~/lib/signInCheck";
import type { ClaudeUsage } from "~/lib/types";

/** After this long (ms) waiting for the account to work, the dialog says it is late. */
const SLOW_MS = 30_000;
/** All steps done: this long (ms) to see them so before the dialog closes. */
const DONE_MS = 800;

export function ImportAccountModalContainer({
  accounts,
  groupNames,
  initialProvider,
  initialGroup,
  replacing,
  glass = false,
  onClose,
  onImported,
}: {
  /** Both providers' accounts as this page shows them now (`ControlState.claudeAccounts`). */
  accounts: ClaudeUsage[];
  /** Pool names from the single `config.groups` list — one list for both providers. */
  groupNames: string[];
  /** Preselected provider tab / pool (the settings group tree's per-group import). */
  initialProvider?: "claude" | "codex";
  initialGroup?: string;
  /** An account this sign-in stands in for. Its provider is the one being signed in, so the
   *  provider tabs go away, and its pools are inherited, so the pool picker does too. */
  replacing?: { provider: "claude" | "codex"; email: string } | null;
  /** In /ng's glass (`ImportAccountModalView`'s `glass`). */
  glass?: boolean;
  onClose: () => void;
  onImported: (email: string) => void;
}) {
  const [provider, setProvider] = useState<"claude" | "codex">(
    replacing?.provider ?? initialProvider ?? "claude",
  );
  const [loginUrl, setLoginUrl] = useState<string | null>(null);
  const [pasted, setPasted] = useState("");
  const [group, setGroup] = useState(initialGroup ?? "");
  const [importing, setImporting] = useState(false);
  // The account the sign-in landed, and the one it took over from (null: none, or itself)
  const [landed, setLanded] = useState<{ email: string; replaced: string | null } | null>(null);
  const [slow, setSlow] = useState(false);
  // Every step is done: the dialog closes (`open` goes false), and the close hands the account
  // to the page, so the exit frames run in front of the page's refresh
  const [finished, setFinished] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const groups = groupNames;

  // A URL per provider, asked for on open and again on a provider switch.
  useEffect(() => {
    let cancelled = false;
    setLoginUrl(null);
    setPasted("");
    setGroup("");
    setError(null);
    beginLogin(provider)
      .then((r) => !cancelled && setLoginUrl(r.url))
      .catch((e: Error) => !cancelled && setError(e.message));
    return () => {
      cancelled = true;
    };
  }, [provider]);

  function submit() {
    if (importing || !pasted.trim()) return;
    setImporting(true);
    setError(null);
    completeLogin(provider, pasted.trim(), group, replacing?.email ?? "")
      .then((r) => {
        const replaced = r.replaced && r.replaced.toLowerCase() !== r.email.toLowerCase() ? r.replaced : null;
        setLanded({ email: r.email, replaced });
      })
      .catch((e: Error) => {
        setError(e.message);
        setImporting(false);
      });
  }

  const check = landed ? checkSignIn(accounts, provider, landed.email, landed.replaced) : null;
  const done = !!check && check.works && check.oldGone;
  useEffect(() => {
    if (!landed) return;
    const t = setTimeout(() => setSlow(true), SLOW_MS);
    return () => clearTimeout(t);
  }, [landed]);
  useEffect(() => {
    if (!done) return;
    const t = setTimeout(() => setFinished(true), DONE_MS);
    return () => clearTimeout(t);
  }, [done]);

  return (
    <ImportAccountModalView
      provider={provider}
      loginUrl={loginUrl}
      pasted={pasted}
      groups={groups}
      group={group}
      replacing={replacing?.email ?? null}
      importing={importing}
      glass={glass}
      progress={landed && check ? { ...landed, ...check, slow } : null}
      error={error}
      onProviderChange={setProvider}
      onPastedChange={setPasted}
      onGroupChange={setGroup}
      open={!finished}
      // Closed before the end, the account is signed in all the same: the page refreshes too
      onClose={() => (landed ? onImported(landed.email) : onClose())}
      onImport={submit}
    />
  );
}
