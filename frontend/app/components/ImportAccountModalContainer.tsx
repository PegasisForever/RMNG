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
  checkClaudeImport,
  checkCodexImport,
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
  const [hostId, setHostId] = useState(() => managed[0]?.id ?? "");
  const [info, setInfo] = useState<ImportCandidate | null>(null);
  const [checking, setChecking] = useState(false);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Re-check the selected clone's login whenever it or the provider changes.
  useEffect(() => {
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
  }, [hostId, provider]);

  function submit() {
    if (!info || importing) return;
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
      clones={managed}
      cloneId={hostId}
      info={info}
      checking={checking}
      importing={importing}
      error={error}
      onProviderChange={setProvider}
      onCloneIdChange={setHostId}
      onClose={onClose}
      onImport={submit}
    />
  );
}
