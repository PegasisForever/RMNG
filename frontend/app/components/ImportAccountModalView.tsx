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
// `glass` draws it in /ng's glass (`~/ng/GLASS.md`), as the Edit layout dialog there: a glass
// panel, flat content on it.
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

/** The dialog's classes, in the dashboard's look and in /ng's (on glass, flat), as the Edit
 *  layout dialog's (`./LayoutEditorView.tsx`). */
const LOOKS = {
  plain: {
    title: "text-sm font-semibold text-slate-900 dark:text-slate-100",
    text: "text-slate-500 dark:text-slate-400",
    strong: "font-medium text-slate-700 dark:text-slate-200",
    input:
      "w-full rounded-md border border-slate-300 px-3 py-2 text-sm text-slate-900 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500",
    open: "border border-slate-300 bg-white text-slate-800 hover:bg-slate-50 dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:hover:bg-slate-700",
    tab: "bg-slate-100 text-slate-600 dark:bg-slate-700 dark:text-slate-300",
    tabOn: "bg-slate-800 text-white dark:bg-slate-600",
    cancel: "rounded-md px-3 py-1.5 text-sm text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800",
    done: "rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40",
    error: "rounded-md bg-rose-50 text-rose-600 dark:bg-rose-950/40 dark:text-rose-400",
    late: "rounded-md bg-amber-50 text-amber-800 dark:bg-amber-950/40 dark:text-amber-300",
    ok: "text-emerald-600 dark:text-emerald-400",
    lateIcon: "text-amber-600 dark:text-amber-400",
    radius: "rounded-lg",
  },
  glass: {
    title: "text-[15px] font-semibold tracking-tight text-neutral-900 dark:text-white",
    text: "text-black/60 dark:text-white/60",
    strong: "font-medium text-neutral-900 dark:text-white",
    input:
      "w-full rounded-[10px] border border-black/10 bg-white/60 px-2.5 py-2 text-sm text-neutral-900 placeholder:text-black/40 focus:border-black/30 focus:outline-none dark:border-white/10 dark:bg-black/30 dark:text-white dark:placeholder:text-white/40 dark:focus:border-white/30",
    open: "border border-black/15 bg-white/50 text-neutral-900 hover:bg-white/80 dark:border-white/15 dark:bg-white/5 dark:text-white dark:hover:bg-white/10",
    tab: "bg-black/5 text-black/60 dark:bg-white/10 dark:text-white/60",
    tabOn: "bg-neutral-900 text-white dark:bg-white dark:text-neutral-900",
    cancel: "rounded-[10px] px-3 py-1.5 text-sm text-black/60 hover:bg-black/5 dark:text-white/60 dark:hover:bg-white/10",
    done: "rounded-[10px] bg-emerald-600 px-5 py-1.5 text-sm font-semibold text-white hover:bg-emerald-500 disabled:opacity-40",
    error: "rounded-[10px] bg-rose-500/15 text-rose-700 dark:text-rose-300",
    late: "rounded-[10px] bg-amber-500/15 text-amber-800 dark:text-amber-300",
    ok: "text-emerald-700 dark:text-emerald-300",
    lateIcon: "text-amber-700 dark:text-amber-300",
    radius: "rounded-[10px]",
  },
};
type Look = (typeof LOOKS)["plain"];

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
  glass = false,
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
  /** In /ng's glass, over the clone's screen. */
  glass?: boolean;
  /** The dialog is finished: unmount it, or hand the account it landed to the page. The exit
   *  frames have already played. */
  onClose: () => void;
  onImport: () => void;
}) {
  const look = glass ? LOOKS.glass : LOOKS.plain;
  const canImport = !importing && pasted.trim().length > 0;
  const site = provider === "codex" ? "ChatGPT" : "Claude";

  return (
    // The `over` rung, because this dialog can be opened FROM the settings panel, which is
    // itself Escape-closable. The Escape stack is LIFO, so whichever dialog mounted last owns
    // the key — this one — and the z order has to match what the stack already decides.
    <ModalShell size="sm" layer="over" open={open} onExited={onClose} glass={glass}>
      {(close) => (
        <>
          {/* The account it is for in the title, so the line under it stays one line */}
          <h3 className={`truncate ${look.title}`} title={replacing ?? undefined}>
            {replacing ? `Sign in again: ${replacing}` : provider === "codex" ? "Add Codex account" : "Add Claude account"}
          </h3>
          {progress ? (
            <Progress p={progress} look={look} onClose={() => close()} />
          ) : (
            <form
              onSubmit={(e) => {
                e.preventDefault();
                if (canImport) onImport();
              }}
            >
              <p className={`mt-1 text-xs ${look.text}`}>Sign in, then paste the address of the page that fails to load.</p>

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
                      className={`rounded-md px-3 py-1 text-sm ${provider === p ? look.tabOn : look.tab}`}
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
                className={`mt-3 flex w-full items-center justify-center gap-1.5 px-3 py-2 text-sm font-medium ${look.radius} ${look.open} ${
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
                className={`mt-2 ${look.input}`}
              />

              {/* Replacing inherits the pools of the account being replaced, so there is
                  nothing to pick here and picking would only fight that. */}
              {replacing || groups.length === 0 ? null : (
                <label className={`mt-3 flex items-center gap-2 text-xs ${look.text}`}>
                  Pool
                  <DropdownSelect
                    rows={[{ value: "", label: "No pool" }, ...groups.map((g) => ({ value: g, label: g }))]}
                    value={group}
                    onChange={onGroupChange}
                    label="Pool"
                    className={`flex-1 ${look.input}`}
                  />
                </label>
              )}

              {error ? <p className={`mt-3 px-3 py-2 text-xs break-words ${look.error}`}>{error}</p> : null}

              <div className="mt-4 flex justify-end gap-2">
                <button type="button" onClick={() => close()} className={look.cancel}>
                  Cancel
                </button>
                <button type="submit" disabled={!canImport} className={look.done}>
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
function Progress({ p, look, onClose }: { p: SignInProgress; look: Look; onClose: () => void }) {
  const strong = (s: string) => <span className={look.strong}>{s}</span>;
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
          <li key={i} className={`flex items-start gap-2 text-xs ${look.text}`}>
            {s.done ? (
              <Check aria-label="done" className={`mt-px size-3.5 shrink-0 ${look.ok}`} />
            ) : p.slow ? (
              <AlertTriangle aria-label="late" className={`mt-px size-3.5 shrink-0 ${look.lateIcon}`} />
            ) : (
              <Loader2 aria-label="in progress" className="mt-px size-3.5 shrink-0 animate-spin opacity-60" />
            )}
            <span className="min-w-0 break-words">{s.done ? s.label : s.doing}</span>
          </li>
        ))}
      </ul>
      {p.slow && !all ? (
        <p className={`mt-3 px-3 py-2 text-xs ${look.late}`}>
          This takes longer than it should. The dialog closes by itself when it is done; you can also close it
          now.
          {p.problem ? <span className="mt-1 block break-words">The server says: {p.problem}</span> : null}
        </p>
      ) : null}
      <div className="mt-4 flex justify-end">
        <button type="button" onClick={onClose} className={look.cancel}>
          {all ? "Done" : "Close"}
        </button>
      </div>
    </div>
  );
}
