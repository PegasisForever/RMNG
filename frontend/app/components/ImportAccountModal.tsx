// Import a Claude account from a clone that's already signed in to Claude Code.
// Flow: pick a clone → the server runs `claude auth status` to confirm it's a
// claude.ai login and shows the account → the server harvests the clone's OAuth
// pair (it owns the refresh lifecycle from then on) and clears the clone's
// credentials file so its Claude Code can't rotate the refresh token.
import { useEffect, useState } from "react";

import { checkClaudeImport, importClaudeAccount } from "~/lib/api";
import type { Host } from "~/lib/types";

type Account = { email: string; orgName: string | null; subscriptionType: string | null };

const input =
  "mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none";

export function ImportAccountModal({
  hosts,
  onClose,
  onImported,
}: {
  hosts: Host[];
  onClose: () => void;
  onImported: (email: string) => void;
}) {
  // Only managed containers (clones) can be imported from.
  const clones = hosts.filter((h) => h.ctid != null);
  const [hostId, setHostId] = useState(() => clones[0]?.id ?? "");
  const [account, setAccount] = useState<Account | null>(null);
  const [checking, setChecking] = useState(false);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Re-check the selected clone's Claude login whenever it changes.
  useEffect(() => {
    if (!hostId) return;
    let cancelled = false;
    setAccount(null);
    setError(null);
    setChecking(true);
    checkClaudeImport(hostId)
      .then((a) => !cancelled && setAccount(a))
      .catch((e: Error) => !cancelled && setError(e.message))
      .finally(() => !cancelled && setChecking(false));
    return () => {
      cancelled = true;
    };
  }, [hostId]);

  const canImport = !!account && !importing;

  function submit() {
    if (!canImport) return;
    setImporting(true);
    setError(null);
    importClaudeAccount(hostId)
      .then((r) => onImported(r.email))
      .catch((e: Error) => {
        setError(e.message);
        setImporting(false);
      });
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4"
      onClick={onClose}
    >
      <div
        className="max-h-[90vh] w-full max-w-md overflow-y-auto rounded-xl border border-slate-200 bg-white p-5 shadow-xl"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <h3 className="text-sm font-semibold text-slate-900">Import Claude account</h3>
        <p className="mt-1 text-xs text-slate-500">
          Harvest a Claude account from a clone that's signed in to Claude Code via claude.ai.
        </p>

        {clones.length === 0 ? (
          <p className="mt-4 rounded-md border border-dashed border-slate-300 p-3 text-center text-xs text-slate-400">
            No clones available to import from.
          </p>
        ) : (
          <>
            <label className="mt-4 block text-xs font-medium text-slate-600">
              Clone
              <select
                value={hostId}
                onChange={(e) => setHostId(e.target.value)}
                className={input}
              >
                {clones.map((h) => (
                  <option key={h.id} value={h.id}>
                    {h.displayName ? `${h.displayName} (${h.id})` : h.id}
                  </option>
                ))}
              </select>
            </label>

            {/* Login status for the selected clone. */}
            <div className="mt-2 min-h-[1.25rem] text-xs">
              {checking ? (
                <span className="text-slate-400">Checking Claude login…</span>
              ) : account ? (
                <span className="text-emerald-700">
                  Signed in: <span className="font-medium">{account.email}</span>
                  {account.subscriptionType ? ` · ${account.subscriptionType}` : ""}
                </span>
              ) : null}
            </div>
          </>
        )}

        {error ? (
          <p className="mt-3 rounded-md bg-rose-50 px-3 py-2 text-xs text-rose-600">{error}</p>
        ) : null}

        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={submit}
            disabled={!canImport}
            className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
          >
            {importing ? "Importing…" : "Import"}
          </button>
        </div>
      </div>
    </div>
  );
}
