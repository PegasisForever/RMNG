// Add-account dialog, markup half. One way in: sign in to the provider here.
//
// Open the URL the server hands back, let the browser land on a dead `localhost` port, and
// paste the address bar back. That address carries the authorization code and there is
// nowhere else it exists, because both providers pin their redirect to the operator's own
// machine and neither accepts another. No clone, no tunnel, no inbound path to the server.
//
// The dialog is the two things the operator does: a button that opens the sign-in page, and
// a box for the address the browser ends on, under one line that says so. A new account also
// picks its provider and its pool: an account in no pool is one the rotator will never hand to
// a clone, which is a strange thing to leave as the default outcome of adding an account. A
// sign-in again has both from the account it repairs (or replaces), so it has neither.
//
// After the exchange the form gives way to a list of steps (`progress`): signed in, the old
// account's pools and clones moved, the new account working on this page, the old one gone.
// The container closes the dialog when the last one is done, so the list behind it is already
// the new one.
//
// Its content is flat on the dialog's panel (`~/desk/GLASS.md`), as the Edit layout dialog's.
//
// It renders from props alone. Every server call lives in ImportAccountModalContainer, and
// every state the operator can reach here is a prop, so all of them are stories.
import { AlertTriangle, Check, ExternalLink, Loader2 } from "lucide-react";
import type { ReactNode } from "react";

import { DropdownSelect } from "~/components/DropdownSelect";
import { ModalShell } from "~/components/ModalShell";
import { BUTTON, DONE_BUTTON, ErrorLine, FIELD, LIT, R_PIECE, TEXT, TEXT_2 } from "~/desk/control/ui";

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

/** The dialog's classes: flat, as the Edit layout dialog's (`./LayoutEditorView.tsx`). */
const LOOK = {
  title: `text-[15px] font-semibold tracking-tight ${TEXT}`,
  text: TEXT_2,
  strong: `font-medium ${TEXT}`,
  input: `w-full ${FIELD} px-2.5 py-2 text-sm placeholder:text-black/40 dark:placeholder:text-white/40`,
  open: BUTTON,
  tab: "bg-black/5 text-black/60 dark:bg-white/10 dark:text-white/60",
  tabOn: LIT,
  cancel: `${BUTTON} px-3 py-1.5 text-sm`,
  done: `${DONE_BUTTON} px-5 py-1.5 text-sm font-semibold`,
  late: `${R_PIECE} bg-amber-500/15 text-amber-800 dark:text-amber-300`,
  ok: "text-emerald-700 dark:text-emerald-300",
  lateIcon: "text-amber-700 dark:text-amber-300",
};

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
  const site = provider === "codex" ? "ChatGPT" : "Claude";

  return (
    <ModalShell size="sm" open={open} onExited={onClose}>
      {(close) => (
        <>
          {/* The account it is for in the title, so the line under it stays one line */}
          <h3 className={`truncate ${LOOK.title}`} data-tip={replacing ?? undefined}>
            {replacing ? `Sign in again: ${replacing}` : provider === "codex" ? "Add Codex account" : "Add Claude account"}
          </h3>
          {progress ? (
            <Progress p={progress} onClose={() => close()} />
          ) : (
            <form
              onSubmit={(e) => {
                e.preventDefault();
                if (canImport) onImport();
              }}
            >
              <p className={`mt-1 text-xs ${LOOK.text}`}>Sign in, then paste the address of the page that fails to load.</p>

              {/* Replacing fixes the provider: it is the dead account's, and a sign-in to the
                  other one could not stand in for it. */}
              {replacing ? null : (
                <div className="mt-3 flex gap-1.5">
                  {(["claude", "codex"] as const).map((p) => (
                    <button
                      key={p}
                      type="button"
                      onClick={() => onProviderChange(p)}
                      aria-pressed={provider === p}
                      className={`rounded-md px-3 py-1 text-sm ${provider === p ? LOOK.tabOn : LOOK.tab}`}
                    >
                      {p === "claude" ? "Claude" : "Codex"}
                    </button>
                  ))}
                </div>
              )}

              <a
                href={loginUrl ?? undefined}
                target="_blank"
                rel="noopener noreferrer"
                aria-disabled={!loginUrl}
                className={`mt-3 flex w-full items-center justify-center gap-1.5 px-3 py-2 text-sm font-medium ${LOOK.open} ${
                  loginUrl ? "" : "pointer-events-none opacity-50"
                }`}
              >
                {loginUrl ? `Open the ${site} sign-in page` : "Preparing the sign-in…"}
                {loginUrl ? <ExternalLink aria-hidden className="size-3.5" /> : null}
              </a>
              <input
                value={pasted}
                onChange={(e) => onPastedChange(e.target.value)}
                aria-label="The address of the page after the sign-in"
                placeholder={
                  provider === "codex" ? "http://localhost:1455/auth/callback?code=…" : "http://localhost:54545/callback?code=…"
                }
                spellCheck={false}
                className={`mt-2 ${LOOK.input}`}
              />

              {/* Replacing inherits the pools of the account being replaced, so there is
                  nothing to pick here and picking would only fight that. */}
              {replacing || groups.length === 0 ? null : (
                <label className={`mt-3 flex items-center gap-2 text-xs ${LOOK.text}`}>
                  Pool
                  <div className="min-w-0 flex-1">
                    <DropdownSelect
                      rows={[{ value: "", label: "No pool" }, ...groups.map((g) => ({ value: g, label: g }))]}
                      value={group}
                      onChange={onGroupChange}
                      label="Pool"
                      className={LOOK.input}
                    />
                  </div>
                </label>
              )}

              {error ? (
                <div className={`mt-3 overflow-hidden ${R_PIECE}`}>
                  <ErrorLine message={error} />
                </div>
              ) : null}

              <div className="mt-4 flex justify-end gap-2">
                <button type="button" onClick={() => close()} className={LOOK.cancel}>
                  Cancel
                </button>
                <button type="submit" disabled={!canImport} className={LOOK.done}>
                  {importing ? "Signing in…" : replacing ? "Sign in again" : "Add account"}
                </button>
              </div>
            </form>
          )}
        </>
      )}
    </ModalShell>
  );
}

/** The steps after the exchange, each done, in progress, or (slow) late. */
function Progress({ p, onClose }: { p: SignInProgress; onClose: () => void }) {
  const strong = (s: string) => <span className={LOOK.strong}>{s}</span>;
  const steps: { done: boolean; label: ReactNode; doing: ReactNode }[] = [
    { done: true, label: <>Signed in as {strong(p.email)}</>, doing: null },
    ...(p.replaced
      ? [{ done: true, label: <>Took over the pools and clones of {strong(p.replaced)}</>, doing: null }]
      : []),
    { done: p.works, label: <>{strong(p.email)} works</>, doing: <>Checking that {strong(p.email)} works…</> },
    ...(p.replaced
      ? [{ done: p.oldGone, label: <>{strong(p.replaced)} removed</>, doing: <>Removing {strong(p.replaced)}…</> }]
      : []),
  ];
  const all = steps.every((s) => s.done);
  return (
    <div className="mt-3">
      <ul aria-label="Sign-in steps" className="space-y-2">
        {steps.map((s, i) => (
          <li key={i} className={`flex items-start gap-2 text-xs ${LOOK.text}`}>
            {s.done ? (
              <Check aria-label="done" className={`mt-px size-3.5 shrink-0 ${LOOK.ok}`} />
            ) : p.slow ? (
              <AlertTriangle aria-label="late" className={`mt-px size-3.5 shrink-0 ${LOOK.lateIcon}`} />
            ) : (
              <Loader2 aria-label="in progress" className="mt-px size-3.5 shrink-0 animate-spin opacity-60" />
            )}
            <span className="min-w-0 break-words">{s.done ? s.label : s.doing}</span>
          </li>
        ))}
      </ul>
      {p.slow && !all ? (
        <p className={`mt-3 px-3 py-2 text-xs ${LOOK.late}`}>
          This takes longer than it should. The dialog closes by itself when it is done; you can also close it
          now.
          {p.problem ? <span className="mt-1 block break-words">The server says: {p.problem}</span> : null}
        </p>
      ) : null}
      <div className="mt-4 flex justify-end">
        <button type="button" onClick={onClose} className={LOOK.cancel}>
          {all ? "Done" : "Close"}
        </button>
      </div>
    </div>
  );
}
