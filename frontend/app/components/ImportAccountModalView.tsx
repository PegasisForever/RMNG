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
// After the exchange the form gives way to a list of steps (`progress`): signed in, the old
// account's pools and clones moved, the new account working on this page, the old one gone.
// The container closes the dialog when the last one is done, so the list behind it is already
// the new one.
//
// It renders from props alone. Every server call lives in ImportAccountModalContainer, and
// every state the operator can reach here is a prop, so all of them are stories.
import { AlertTriangle, Check, ExternalLink, Loader2 } from "lucide-react";
import type { ReactNode } from "react";

import { DropdownSelect } from "~/components/DropdownSelect";
import { ModalShell } from "~/components/ModalShell";

/** After the sign-in: the account it landed, and how far this page is with it. */
export type SignInProgress = {
  email: string;
  /** The account it took over from; null when it replaced none, or repaired itself. */
  replaced: string | null;
  /** The account list here shows it with fresh numbers and a token that works. */
  works: boolean;
  /** The replaced account is out of the list. */
  oldGone: boolean;
  /** It takes longer than it should: the dialog says so. */
  slow: boolean;
  /** What the list says is wrong with the account, while it does not work. */
  problem: string | null;
};

const input =
  "mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500";

export function ImportAccountModalView({
  provider,
  loginUrl,
  pasted,
  groups,
  group,
  replacing = null,
  importing,
  progress = null,
  error,
  onProviderChange,
  onPastedChange,
  onGroupChange,
  open = true,
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
  /** The email this sign-in stands in for, or null for a plain import. Both choices above
   *  are already made in this mode: the provider is that account's, and the pools are the
   *  ones it sits in, so neither control is drawn. */
  replacing?: string | null;
  /** The exchange is in flight. */
  importing: boolean;
  /** The exchange is done: the steps after it, in place of the form. */
  progress?: SignInProgress | null;
  error: string | null;
  onProviderChange: (provider: "claude" | "codex") => void;
  onPastedChange: (pasted: string) => void;
  onGroupChange: (group: string) => void;
  /** The dialog is on screen. The container flips it false when the sign-in lands, and the
   *  dialog plays its exit before the unmount rather than blinking out. */
  open?: boolean;
  /** The dialog is finished: unmount it, or hand the account it landed to the page. The exit
   *  frames have already played. */
  onClose: () => void;
  onImport: () => void;
}) {
  const canImport = !importing && pasted.trim().length > 0;

  return (
    // The `over` rung, because this dialog can be opened FROM the settings panel, which is
    // itself Escape-closable. The Escape stack is LIFO, so whichever dialog mounted last owns
    // the key — this one — and the z order has to match what the stack already decides.
    <ModalShell size="sm" layer="over" open={open} onExited={onClose}>
      {(close) => (
        <>
          <h3 className="text-sm font-semibold text-slate-900 dark:text-slate-100">
            {replacing ? "Sign in again" : provider === "codex" ? "Add Codex account" : "Add Claude account"}
          </h3>
          {progress ? (
            <Progress p={progress} onClose={() => close()} />
          ) : (
            <>
              <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
                {replacing ? (
                  <>
                    The sign-in of <Strong>{replacing}</Strong> no longer works, so no clone can use
                    it. Sign in again to repair it.
                  </>
                ) : (
                  <>
                    Sign in to the provider here. This server keeps the account and
                    hands short-lived tokens to clones.
                  </>
                )}
              </p>
              {replacing ? (
                <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
                  To use another account in its place, sign in as that account. It takes over the
                  pools and clones of {replacing}, and {replacing} is removed.
                </p>
              ) : null}

              {/* Replacing fixes the provider: it is the dead account's, and a sign-in to the other
                one could not stand in for it. */}
              {replacing ? null : (
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
              )}

              {loginUrl ? (
                <>
                  <p className={`text-xs text-slate-600 dark:text-slate-300 ${replacing ? "mt-3" : ""}`}>
                    1. Open the sign-in page, and sign in{replacing ? ` as ${replacing} (or the other account)` : ""}:
                  </p>
                  <a
                    href={loginUrl}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="mt-1.5 inline-flex items-center gap-1.5 rounded-md border border-slate-300 bg-white px-3 py-1.5 text-sm font-medium text-slate-800 hover:bg-slate-50 dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:hover:bg-slate-700"
                  >
                    Open the {provider === "codex" ? "ChatGPT" : "Claude"} sign-in page
                    <ExternalLink aria-hidden className="size-3.5" />
                  </a>
                  <p className="mt-1 break-all text-[11px] text-slate-400 dark:text-slate-500">
                    Or copy its address: {loginUrl}
                  </p>
                  <p className="mt-3 text-xs text-slate-600 dark:text-slate-300">
                    2. After the sign-in, the browser goes to a page that does not load. That is
                    expected. Copy the whole address of that page and paste it here:
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
                <p className="text-xs text-slate-400 dark:text-slate-500">
                  Preparing the sign-in…
                </p>
              )}

              {/* Replacing inherits the pools of the account being replaced, so there is nothing to
                pick here and picking would only fight that. */}
              {replacing ? null : (
                <>
                  <label className="mt-4 block text-xs font-medium text-slate-600 dark:text-slate-300">
                    Pool
                    <DropdownSelect
                      rows={[
                        { value: "", label: "No pool" },
                        ...groups.map((g) => ({ value: g, label: g })),
                      ]}
                      value={group}
                      onChange={onGroupChange}
                      disabled={groups.length === 0}
                      label="Pool"
                      className={input}
                    />
                  </label>
                  <p className="mt-1 text-xs text-slate-400 dark:text-slate-500">
                    {groups.length === 0
                      ? "No pools configured for this provider. The account can still be pinned to a clone by name."
                      : "A clone bound to this pool can be handed the account by the rotator. Outside a pool it has to be pinned by name."}
                  </p>
                </>
              )}

              {error ? (
                <p className="mt-3 rounded-md bg-rose-50 px-3 py-2 text-xs text-rose-600 dark:bg-rose-950/40 dark:text-rose-400">
                  {error}
                </p>
              ) : null}

              <div className="mt-4 flex justify-end gap-2">
                <button
                  type="button"
                  onClick={() => close()}
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
                  {importing ? "Signing in…" : replacing ? "Sign in again" : "Add account"}
                </button>
              </div>
            </>
          )}
        </>
      )}
    </ModalShell>
  );
}

function Strong({ children }: { children: ReactNode }) {
  return <span className="font-medium text-slate-700 dark:text-slate-200">{children}</span>;
}

/** The steps after the exchange, each done, in progress, or (slow) late. */
function Progress({ p, onClose }: { p: SignInProgress; onClose: () => void }) {
  const steps: { done: boolean; label: ReactNode; doing: ReactNode }[] = [
    { done: true, label: <>Signed in as <Strong>{p.email}</Strong></>, doing: null },
    ...(p.replaced
      ? [{ done: true, label: <>Took over the pools and clones of <Strong>{p.replaced}</Strong></>, doing: null }]
      : []),
    {
      done: p.works,
      label: <><Strong>{p.email}</Strong> works</>,
      doing: <>Checking that <Strong>{p.email}</Strong> works…</>,
    },
    ...(p.replaced
      ? [{ done: p.oldGone, label: <><Strong>{p.replaced}</Strong> removed</>, doing: <>Removing <Strong>{p.replaced}</Strong>…</> }]
      : []),
  ];
  const all = steps.every((s) => s.done);
  return (
    <div className="mt-3">
      <ul aria-label="Sign-in steps" className="space-y-2">
        {steps.map((s, i) => (
          <li key={i} className="flex items-start gap-2 text-xs text-slate-600 dark:text-slate-300">
            {s.done ? (
              <Check aria-label="done" className="mt-px size-3.5 shrink-0 text-emerald-600 dark:text-emerald-400" />
            ) : p.slow ? (
              <AlertTriangle aria-label="late" className="mt-px size-3.5 shrink-0 text-amber-600 dark:text-amber-400" />
            ) : (
              <Loader2 aria-label="in progress" className="mt-px size-3.5 shrink-0 animate-spin text-slate-400" />
            )}
            <span className="min-w-0 break-words">{s.done ? s.label : s.doing}</span>
          </li>
        ))}
      </ul>
      {p.slow && !all ? (
        <p className="mt-3 rounded-md bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
          This takes longer than it should. The dialog closes by itself when it is done; you can
          also close it now.
          {p.problem ? <span className="mt-1 block break-words">The server says: {p.problem}</span> : null}
        </p>
      ) : null}
      <div className="mt-4 flex justify-end">
        <button
          type="button"
          onClick={onClose}
          className="rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800"
        >
          {all ? "Done" : "Close"}
        </button>
      </div>
    </div>
  );
}
