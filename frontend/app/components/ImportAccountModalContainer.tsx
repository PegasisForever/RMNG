// Import a Claude or Codex account from a clone that's already signed in to its agent CLI.
// Flow: pick a clone → the server runs `claude auth status` to confirm it's a claude.ai login
// and shows the account → the server harvests the clone's OAuth pair (it owns the refresh
// lifecycle from then on) and clears the clone's credentials file so its Claude Code can't
// rotate the refresh token.
//
// Both server calls live here, one per step, and nothing below knows there is a server at
// all. The markup is ImportAccountModalView.
import { useEffect, useState } from "react";

import {
  ImportAccountModalView,
  type ImportCandidate,
} from "~/components/ImportAccountModalView";
import {
  beginLogin,
  checkClaudeImport,
  checkCodexImport,
  completeLogin,
  importClaudeAccount,
  importCodexAccount,
} from "~/lib/api";
import type { Clone } from "~/lib/types";

export function ImportAccountModalContainer({
  clones,
  onClose,
  onImported,
}: {
  clones: Clone[];
  onClose: () => void;
  onImported: (email: string) => void;
}) {
  // Only managed containers can be imported from.
  const managed = clones.filter((h) => h.managed);
  const [provider, setProvider] = useState<"claude" | "codex">("claude");
  const [mode, setMode] = useState<"clone" | "login">("clone");
  const [loginUrl, setLoginUrl] = useState<string | null>(null);
  const [pasted, setPasted] = useState("");
  const [hostId, setHostId] = useState(() => managed[0]?.id ?? "");
  const [info, setInfo] = useState<ImportCandidate | null>(null);
  const [checking, setChecking] = useState(false);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Ask for a sign-in URL whenever this dialog is on the sign-in path. Re-asked on a
  // provider change, since the two flows are separate logins with separate verifiers, and
  // the one already waiting is simply abandoned: it expires on its own.
  useEffect(() => {
    if (mode !== "login") return;
    let cancelled = false;
    setLoginUrl(null);
    setPasted("");
    setError(null);
    beginLogin(provider)
      .then((r) => !cancelled && setLoginUrl(r.url))
      .catch((e: Error) => !cancelled && setError(e.message));
    return () => {
      cancelled = true;
    };
  }, [mode, provider]);

  // Re-check the selected clone's login whenever it or the provider changes.
  useEffect(() => {
    if (mode !== "clone") return;
    if (!hostId) return;
    let cancelled = false;
    setInfo(null);
    setError(null);
    setChecking(true);
    const check = provider === "codex" ? checkCodexImport : checkClaudeImport;
    check(hostId)
      .then((r) => {
        // codex returns { email, plan }, claude returns { email, subscriptionType }.
        const plan = "plan" in r ? r.plan : (r as { subscriptionType: string | null }).subscriptionType;
        if (!cancelled) setInfo({ email: r.email, plan });
      })
      .catch((e: Error) => !cancelled && setError(e.message))
      .finally(() => !cancelled && setChecking(false));
    return () => {
      cancelled = true;
    };
  }, [hostId, provider, mode]);

  function submit() {
    if (importing) return;
    if (mode === "login") {
      if (!pasted.trim()) return;
      setImporting(true);
      setError(null);
      completeLogin(provider, pasted.trim())
        .then((r) => onImported(r.email))
        .catch((e: Error) => {
          setError(e.message);
          setImporting(false);
        });
      return;
    }
    if (!info) return;
    setImporting(true);
    setError(null);
    const doImport = provider === "codex" ? importCodexAccount : importClaudeAccount;
    doImport(hostId)
      .then((r) => onImported(r.email))
      .catch((e: Error) => {
        setError(e.message);
        setImporting(false);
      });
  }

  return (
    <ImportAccountModalView
      provider={provider}
      mode={mode}
      loginUrl={loginUrl}
      pasted={pasted}
      clones={managed}
      cloneId={hostId}
      info={info}
      checking={checking}
      importing={importing}
      error={error}
      onProviderChange={setProvider}
      onModeChange={setMode}
      onCloneIdChange={setHostId}
      onPastedChange={setPasted}
      onClose={onClose}
      onImport={submit}
    />
  );
}
