// Import-account dialog, markup half. Two ways in, and the operator picks one:
//
// - From a clone that is already signed in. Read back who it is signed in as, then take that
//   account over.
// - By signing in here. Open the URL the server hands back, let the browser land on a dead
//   `localhost` port, and paste the address bar back. That address carries the authorization
//   code, and there is nowhere else it exists: both providers pin their redirect to the
//   operator's own machine, so no clone and no tunnel is involved either way.
//
// It renders from props alone. Every server call lives in ImportAccountModalContainer, and
// every state the operator can reach here is a prop, so all of them are stories.
import type { Clone } from "~/lib/types";
import { useModalEscape } from "~/lib/useModalEscape";

const input =
  "mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500";

/** Who the selected clone is signed in as, as the check route reports it. */
export interface ImportCandidate {
  email: string;
  /** Subscription or plan name, when the provider gives one. */
  plan: string | null;
}

export function ImportAccountModalView({
  provider,
  mode,
  clones,
  cloneId,
  info,
  checking,
  importing,
  error,
  loginUrl,
  pasted,
  onProviderChange,
  onModeChange,
  onCloneIdChange,
  onPastedChange,
  onClose,
  onImport,
}: {
  provider: "claude" | "codex";
  /** Which way in. `"clone"` harvests a signed-in clone, `"login"` signs in here. */
  mode: "clone" | "login";
  /** The clones that can be imported from. The container has already dropped the unmanaged
   *  ones, so an empty list here means there is genuinely nowhere to import from. */
  clones: Clone[];
  cloneId: string;
  /** The selected clone's login, once the check comes back. Null while unknown. */
  info: ImportCandidate | null;
  /** A login check is in flight. */
  checking: boolean;
  /** The import itself is in flight. */
  importing: boolean;
  error: string | null;
  /** The URL to open, once `begin` has answered. Null before that and in clone mode. */
  loginUrl: string | null;
  /** What the operator pasted back. */
  pasted: string;
  onProviderChange: (provider: "claude" | "codex") => void;
  onModeChange: (mode: "clone" | "login") => void;
  onCloneIdChange: (cloneId: string) => void;
  onPastedChange: (pasted: string) => void;
  onClose: () => void;
  onImport: () => void;
}) {
  const canImport =
    !importing && (mode === "clone" ? !!info : pasted.trim().length > 0);

  // Escape closes regardless of focus — a document-level listener since the backdrop click no
  // longer does (see below), and since nothing here autofocuses: on open the focus is still on
  // the button that launched this, so a React `onKeyDown` on the panel would never fire.
  //
  // Stacking matters here specifically: this dialog can be opened FROM the settings panel,
  // which is itself Escape-closable. The stack is LIFO, so whichever modal mounted last owns
  // Escape — this one, since it mounts on top. Its z-60 must stay above the panel's z-50 to
  // match, or Escape would dismiss the dialog you can't see.
  useModalEscape(onClose);

  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center bg-slate-900/30 p-4">
      {/* Backdrop is inert — clicking it must not close the dialog, only Cancel/Escape do. */}
      <div className="max-h-[90vh] w-full max-w-md overflow-y-auto rounded-xl border border-slate-200 bg-white p-5 shadow-xl dark:border-slate-700 dark:bg-slate-800">
        <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
          {provider === "codex" ? "Import Codex account" : "Import Claude account"}
        </h3>
        <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
          {mode === "clone"
            ? "Take over the account a clone is already signed in to."
            : "Sign in to the provider here. No clone is involved."}
        </p>

        <div className="mb-3 flex gap-2">
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

        <div className="mb-3 flex gap-2 text-xs">
          {(["clone", "login"] as const).map((m) => (
            <button
              key={m}
              type="button"
              onClick={() => onModeChange(m)}
              className={
                "rounded px-2.5 py-1 " +
                (mode === m
                  ? "bg-emerald-600 text-white"
                  : "bg-slate-100 text-slate-600 dark:bg-slate-700 dark:text-slate-300")
              }
            >
              {m === "clone" ? "From a clone" : "Sign in here"}
            </button>
          ))}
        </div>

        {mode === "login" ? (
          <div className="mt-4">
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
                  placeholder="http://localhost:54545/callback?code=…"
                  spellCheck={false}
                  className={input}
                />
              </>
            ) : (
              <p className="text-xs text-slate-400 dark:text-slate-500">Preparing the sign-in…</p>
            )}
          </div>
        ) : clones.length === 0 ? (
          <p className="mt-4 rounded-md border border-dashed border-slate-300 p-3 text-center text-xs text-slate-400 dark:border-slate-600 dark:text-slate-500">
            No clones available to import from.
          </p>
        ) : (
          <>
            <label className="mt-4 block text-xs font-medium text-slate-600 dark:text-slate-300">
              Clone
              <select
                value={cloneId}
                onChange={(e) => onCloneIdChange(e.target.value)}
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
            onClick={onImport}
            disabled={!canImport}
            className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
          >
            {importing ? "Importing…" : mode === "login" ? "Finish sign-in" : "Import"}
          </button>
        </div>
      </div>
    </div>
  );
}
