// Add an account to a group by completing that group's CLIProxyAPI OAuth login.
//
// The group instance runs headless in the server container, so its OAuth callback
// redirects to a `localhost` URL on the OPERATOR'S machine (Claude → :54545, Codex →
// :1455, Gemini/Antigravity → :51121) that won't load. The flow is therefore: start → the operator opens the login
// URL and signs in → the browser lands on a dead localhost URL → the operator copies
// that full URL back here → complete → poll until the credential lands in the auth-dir.
import { useEffect, useRef, useState } from "react";

import {
  completeGroupLogin,
  groupLoginStatus,
  startGroupLogin,
  type LoginProvider,
} from "~/lib/api";

const input =
  "mt-1 w-full rounded-md border border-slate-300 px-3 py-2 text-sm font-normal text-slate-900 placeholder:text-slate-400 focus:border-emerald-500 focus:outline-none dark:border-slate-600 dark:bg-slate-800 dark:text-slate-100 dark:placeholder:text-slate-500";

type Phase = "start" | "await-redirect" | "completing" | "done";

/** Poll cadence + cap while waiting for the instance to exchange the code and save the
 *  credential. CLIProxyAPI keeps a completed session queryable for ~1 min, so 1.5s × 40
 *  (~60s) comfortably catches the `done` transition while bounding a stuck exchange. */
const STATUS_POLL_MS = 1500;
const MAX_STATUS_POLLS = 40;

export function GroupLoginModal({
  group,
  onClose,
  onDone,
}: {
  /** The group whose CLIProxyAPI instance the account logs into. */
  group: string;
  onClose: () => void;
  /** Called after the credential lands (so the parent can nudge a state refresh). */
  onDone: () => void;
}) {
  const [provider, setProvider] = useState<LoginProvider>("anthropic");
  const [phase, setPhase] = useState<Phase>("start");
  const [loginUrl, setLoginUrl] = useState("");
  const [state, setState] = useState("");
  const [redirectUrl, setRedirectUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => () => {
    if (pollRef.current) clearInterval(pollRef.current);
  }, []);

  async function start() {
    setBusy(true);
    setError(null);
    try {
      const r = await startGroupLogin(group, provider);
      setLoginUrl(r.url);
      setState(r.state);
      setPhase("await-redirect");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function complete() {
    const redirect = redirectUrl.trim();
    if (!redirect) return;
    setBusy(true);
    setError(null);
    setPhase("completing");
    try {
      await completeGroupLogin(group, { provider, redirectUrl: redirect });
      // The instance now exchanges the code and writes the credential to its auth-dir in the
      // background. Poll `login/status` (normalized to `pending`/`done`/`error`) until it
      // reports `done`; a real `error` stops immediately, and the cap bounds a stuck exchange.
      await new Promise<void>((resolve, reject) => {
        let tries = 0;
        const stop = () => {
          if (pollRef.current) clearInterval(pollRef.current);
          pollRef.current = null;
        };
        pollRef.current = setInterval(async () => {
          tries += 1;
          try {
            const status = await groupLoginStatus(group, state);
            if (status.state === "done") {
              stop();
              resolve();
            } else if (status.state === "error") {
              stop();
              reject(new Error(status.error || "Authentication failed"));
            } else if (tries >= MAX_STATUS_POLLS) {
              stop();
              reject(
                new Error(
                  "Timed out waiting for the login to finish. If the account doesn't appear, try again.",
                ),
              );
            }
            // otherwise "pending" → keep polling
          } catch (e) {
            // Transient failure reaching the instance; keep trying, but give up at the cap.
            if (tries >= MAX_STATUS_POLLS) {
              stop();
              reject(e instanceof Error ? e : new Error(String(e)));
            }
          }
        }, STATUS_POLL_MS);
      });
      setPhase("done");
      onDone();
    } catch (e) {
      setError((e as Error).message);
      setPhase("await-redirect"); // let the operator retry the paste
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center bg-slate-900/40 p-4"
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
          Add account to <span className="text-emerald-700 dark:text-emerald-400">{group}</span>
        </h3>
        <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
          Logs an account into this group's proxy via OAuth. The account is added to this
          group only (independent tokens per group).
        </p>

        {phase === "start" ? (
          <>
            <div className="mt-4 flex gap-2">
              {(["anthropic", "codex", "antigravity"] as const).map((p) => (
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
                  {p === "anthropic"
                    ? "Claude (Anthropic)"
                    : p === "codex"
                      ? "Codex (OpenAI)"
                      : "Gemini (Antigravity)"}
                </button>
              ))}
            </div>
            <button
              type="button"
              onClick={start}
              disabled={busy}
              className="mt-4 w-full rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
            >
              {busy ? "Starting…" : "Start login"}
            </button>
          </>
        ) : null}

        {phase === "await-redirect" || phase === "completing" ? (
          <div className="mt-4 space-y-3">
            <div className="rounded-md border border-slate-200 bg-slate-50 p-3 text-xs dark:border-slate-700 dark:bg-slate-900/40">
              <p className="font-medium text-slate-700 dark:text-slate-200">1 · Open and sign in</p>
              <a
                href={loginUrl}
                target="_blank"
                rel="noreferrer"
                className="mt-1 block break-all text-emerald-700 underline dark:text-emerald-400"
              >
                {loginUrl}
              </a>
              <p className="mt-2 text-slate-500 dark:text-slate-400">
                After signing in you'll be redirected to a <code>localhost</code> URL that won't
                load — that's expected. Copy the <em>full</em> URL from your browser's address bar
                and paste it below.
              </p>
            </div>
            <label className="block text-xs font-medium text-slate-600 dark:text-slate-300">
              2 · Paste the redirected URL
              <input
                value={redirectUrl}
                onChange={(e) => setRedirectUrl(e.target.value)}
                placeholder="http://localhost:54545/callback?code=…&state=…"
                spellCheck={false}
                className={input}
              />
            </label>
          </div>
        ) : null}

        {phase === "done" ? (
          <p className="mt-4 rounded-md bg-emerald-50 px-3 py-2 text-xs text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-400">
            Account added — its usage should appear under the group in a moment.
          </p>
        ) : null}

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
            {phase === "done" ? "Close" : "Cancel"}
          </button>
          {phase === "await-redirect" || phase === "completing" ? (
            <button
              type="button"
              onClick={complete}
              disabled={busy || !redirectUrl.trim()}
              className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-40"
            >
              {phase === "completing" ? "Completing…" : "Complete login"}
            </button>
          ) : null}
        </div>
      </div>
    </div>
  );
}
