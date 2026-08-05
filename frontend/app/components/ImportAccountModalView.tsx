// Add-account dialog, markup half. One way in: sign in to the provider here.
//
// Open the URL the server hands back, let the browser land on a dead `localhost` port, and
// paste the address bar back. That address carries the authorization code and there is
// nowhere else it exists, because both providers pin their redirect to the operator's own
// machine and neither accepts another. No clone, no tunnel, no inbound path to the server.
//
// The pool picker is part of the import rather than a step after it: an account in no pool
// is one the rotator will never hand to a clone, which is a strange thing to leave as the
// default outcome of adding an account.
//
// It renders from props alone. Every server call lives in ImportAccountModalContainer, and
// every state the operator can reach here is a prop, so all of them are stories.
import { useModalEscape } from "~/lib/useModalEscape";

const input =
  "mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500";

export function ImportAccountModalView({
  provider,
  loginUrl,
  pasted,
  groups,
  group,
  importing,
  error,
  onProviderChange,
  onPastedChange,
  onGroupChange,
  onClose,
  onImport,
}: {
  provider: "claude" | "codex";
  /** The URL to open, once the server has answered. Null while it is being asked for. */
  loginUrl: string | null;
  /** What the operator pasted back. */
  pasted: string;
  /** The pools configured for this provider. Empty means there are none to join. */
  groups: string[];
  /** The pool to join, or `""` for none. */
  group: string;
  /** The exchange is in flight. */
  importing: boolean;
  error: string | null;
  onProviderChange: (provider: "claude" | "codex") => void;
  onPastedChange: (pasted: string) => void;
  onGroupChange: (group: string) => void;
  onClose: () => void;
  onImport: () => void;
}) {
  const canImport = !importing && pasted.trim().length > 0;

  // Escape closes regardless of focus: a document-level listener, since the backdrop click
  // does not close and nothing here autofocuses. On open the focus is still on the button
  // that launched this, so a React `onKeyDown` on the panel would never fire.
  //
  // Stacking matters. This dialog can be opened FROM the settings panel, which is itself
  // Escape-closable. The stack is LIFO, so whichever modal mounted last owns Escape, which
  // is this one. Its z-60 must stay above the panel's z-50 to match.
  useModalEscape(onClose);

  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center bg-slate-900/30 p-4">
      {/* Backdrop is inert: clicking it must not close the dialog, only Cancel and Escape do. */}
      <div className="max-h-[90vh] w-full max-w-md overflow-y-auto rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800">
        <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
          {provider === "codex" ? "Add Codex account" : "Add Claude account"}
        </h3>
        <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
          Sign in to the provider here. This server keeps the account and hands short-lived
          tokens to clones.
        </p>

        <div className="my-3 flex gap-2">
          {(["claude", "codex"] as const).map((p) => (
            <button
              key={p}
              type="button"
              onClick={() => onProviderChange(p)}
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

        {loginUrl ? (
          <>
            <p className="text-xs text-slate-600 dark:text-slate-300">
              1. Open this URL and sign in:
            </p>
            <a
              href={loginUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="mt-1 block break-all rounded-md bg-slate-50 px-3 py-2 text-xs text-emerald-700 underline dark:bg-slate-900 dark:text-emerald-400"
            >
              {loginUrl}
            </a>
            <p className="mt-3 text-xs text-slate-600 dark:text-slate-300">
              2. The page it lands on will fail to load. That is expected. Copy its whole
              address and paste it here:
            </p>
            <input
              value={pasted}
              onChange={(e) => onPastedChange(e.target.value)}
              placeholder={
                provider === "codex"
                  ? "http://localhost:1455/auth/callback?code=…"
                  : "http://localhost:54545/callback?code=…"
              }
              spellCheck={false}
              className={input}
            />
          </>
        ) : (
          <p className="text-xs text-slate-400 dark:text-slate-500">Preparing the sign-in…</p>
        )}

        <label className="mt-4 block text-xs font-medium text-slate-600 dark:text-slate-300">
          Pool
          <select
            value={group}
            onChange={(e) => onGroupChange(e.target.value)}
            disabled={groups.length === 0}
            className={input}
          >
            <option value="">No pool</option>
            {groups.map((g) => (
              <option key={g} value={g}>
                {g}
              </option>
            ))}
          </select>
        </label>
        <p className="mt-1 text-xs text-slate-400 dark:text-slate-500">
          {groups.length === 0
            ? "No pools configured for this provider. The account can still be pinned to a clone by name."
            : "A clone bound to this pool can be handed the account by the rotator. Outside a pool it has to be pinned by name."}
        </p>

        {error ? (
          <p className="mt-3 rounded-md bg-rose-50 px-3 py-2 text-xs text-rose-600 dark:bg-rose-950/40 dark:text-rose-400">
            {error}
          </p>
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
            onClick={onImport}
            disabled={!canImport}
            className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
          >
            {importing ? "Finishing…" : "Finish sign-in"}
          </button>
        </div>
      </div>
    </div>
  );
}
