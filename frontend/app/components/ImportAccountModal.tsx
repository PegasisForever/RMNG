// Import a Claude account from a clone that's already signed in to Claude Code.
// Flow: pick a clone → the server runs `claude auth status` to confirm it's a
// claude.ai login and shows the account → the server harvests the clone's OAuth
// pair (it owns the refresh lifecycle from then on) and clears the clone's
// credentials file so its Claude Code can't rotate the refresh token.
import { useEffect, useState } from "react";

import {
  checkClaudeImport,
  checkCodexImport,
  importClaudeAccount,
  importCodexAccount,
} from "~/lib/api";
import type { Host } from "~/lib/types";

const input =
  "mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500";

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
  const clones = hosts.filter((h) => h.managed);
  const [provider, setProvider] = useState<"claude" | "codex">("claude");
  const [hostId, setHostId] = useState(() => clones[0]?.id ?? "");
  const [info, setInfo] = useState<{ email: string; plan: string | null } | null>(null);
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

  const canImport = !!info && !importing;

  function submit() {
    if (!canImport) return;
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
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/30 p-4"
      onClick={onClose}
    >
      <div
        className="max-h-[90vh] w-full max-w-md overflow-y-auto rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
          {provider === "codex" ? "Import Codex account" : "Import Claude account"}
        </h3>
        <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
          Harvest a Claude account from a clone that's signed in to Claude Code via claude.ai.
        </p>

        <div className="mb-3 flex gap-2">
          {(["claude", "codex"] as const).map((p) => (
            <button
              key={p}
              type="button"
              onClick={() => setProvider(p)}
              className={
                "rounded px-3 py-1 text-sm " +
                (provider === p
                  ? "bg-slate-800 text-white dark:bg-slate-600 dark:text-white"
                  : "bg-slate-100 text-slate-600 dark:bg-slate-700 dark:text-slate-300")
              }
            >
              {p === "claude" ? "Claude" : "Codex"}
            </button>
          ))}
        </div>

        {clones.length === 0 ? (
          <p className="mt-4 rounded-md border border-dashed border-slate-300 p-3 text-center text-xs text-slate-400 dark:border-slate-600 dark:text-slate-500">
            No clones available to import from.
          </p>
        ) : (
          <>
            <label className="mt-4 block text-xs font-medium text-slate-600 dark:text-slate-300">
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
                <span className="text-slate-400 dark:text-slate-500">Checking {provider === "codex" ? "Codex" : "Claude"} login…</span>
              ) : info ? (
                <span className="text-emerald-700 dark:text-emerald-400">
                  Signed in: <span className="font-medium">{info.email}</span>
                  {info.plan ? ` · ${info.plan}` : ""}
                </span>
              ) : null}
            </div>
          </>
        )}

        {error ? (
          <p className="mt-3 rounded-md bg-rose-50 px-3 py-2 text-xs text-rose-600 dark:bg-rose-950/40 dark:text-rose-400">{error}</p>
        ) : null}

        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800"
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
