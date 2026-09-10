// Add a Claude or Codex account by signing in to the provider from this page.
//
// Two server calls, one per step. `begin` mints a PKCE verifier and hands back the URL to
// open; `complete` takes whatever the browser landed on, redeems the code, stores the
// account and joins it to the chosen pool. Nothing below knows there is a server at all.
//
// Switching provider starts a fresh sign-in, because the two are separate logins with
// separate verifiers. The one already waiting is simply abandoned: it expires on its own,
// and it names no account, so nothing is left behind by walking away from it.
//
// The markup is ImportAccountModalView.
import { useEffect, useState } from "react";

import { ImportAccountModalView } from "~/components/ImportAccountModalView";
import { beginLogin, completeLogin } from "~/lib/api";

export function ImportAccountModalContainer({
  groupNames,
  initialProvider,
  initialGroup,
  replacing,
  onClose,
  onImported,
}: {
  /** Pool names from the single `config.groups` list — one list for both providers. */
  groupNames: string[];
  /** Preselected provider tab / pool (the settings group tree's per-group import). */
  initialProvider?: "claude" | "codex";
  initialGroup?: string;
  /** An account this sign-in stands in for. Its provider is the one being signed in, so the
   *  provider tabs go away, and its pools are inherited, so the pool picker does too. */
  replacing?: { provider: "claude" | "codex"; email: string } | null;
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
      .then((r) => onImported(r.email))
      .catch((e: Error) => {
        setError(e.message);
        setImporting(false);
      });
  }

  return (
    <ImportAccountModalView
      provider={provider}
      loginUrl={loginUrl}
      pasted={pasted}
      groups={groups}
      group={group}
      replacing={replacing?.email ?? null}
      importing={importing}
      error={error}
      onProviderChange={setProvider}
      onPastedChange={setPasted}
      onGroupChange={setGroup}
      onClose={onClose}
      onImport={submit}
    />
  );
}
