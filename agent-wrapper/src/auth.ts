// The clone's Codex credential, read straight from the file the control-server pushes.
//
// The control-server owns every Codex token. It signs in at the server, refreshes ~2h
// before expiry, and re-pushes `~/.codex/auth.json` over `docker exec` (control-server
// codex.rs). The pushed file carries an EMPTY refresh_token on purpose, so the clone can
// never rotate the single-use pair the server holds.
//
// pi would normally refresh a stored OAuth credential itself once it is within five
// minutes of expiry. With no refresh token that call can only fail, so this store reports
// a far-future expiry and pi never enters that path. The Claude side uses the same trick
// (control-server claude.rs `credentials_json`, expiresAt 4102444800000).
//
// `read` parses the file on every call, so a freshly pushed token takes effect without
// restarting the wrapper. That matches how the Codex CLI and Claude Code behave.

import { readFileSync } from "node:fs";

import type {
  AuthOperationOptions,
  Credential,
  CredentialInfo,
  CredentialStore,
} from "@earendil-works/pi-ai";

/** pi's provider id for the ChatGPT-subscription Codex backend. */
export const CODEX_PROVIDER_ID = "openai-codex";

/** 2100-01-01. Far enough out that pi never tries to refresh. */
const NEVER_EXPIRES = 4102444800000;

/** The shape the control-server writes. `refresh_token` is always empty. */
interface CodexAuthFile {
  tokens?: {
    access_token?: string;
    account_id?: string;
  };
}

/**
 * Build the pi credential from the pushed Codex auth file. Returns undefined when the file
 * is missing, unreadable, or has no access token — the clone simply has no Codex account
 * assigned, which is a normal state the control-server produces with `clear_clone_token`.
 */
export function readCodexCredential(path: string): Credential | undefined {
  let parsed: CodexAuthFile;
  try {
    parsed = JSON.parse(readFileSync(path, "utf8")) as CodexAuthFile;
  } catch {
    return undefined;
  }
  const access = parsed?.tokens?.access_token;
  const accountId = parsed?.tokens?.account_id;
  if (typeof access !== "string" || !access) return undefined;
  return {
    type: "oauth",
    access,
    refresh: "",
    expires: NEVER_EXPIRES,
    ...(typeof accountId === "string" && accountId ? { accountId } : {}),
  };
}

/**
 * A read-only CredentialStore over `~/.codex/auth.json`. Writes are accepted and dropped:
 * the file belongs to the control-server, and a wrapper-side write would be overwritten by
 * the next push anyway.
 */
export class CodexFileCredentialStore implements CredentialStore {
  constructor(private readonly authPath: string) {}

  async read(providerId: string, options?: AuthOperationOptions): Promise<Credential | undefined> {
    options?.signal?.throwIfAborted();
    if (providerId !== CODEX_PROVIDER_ID) return undefined;
    return readCodexCredential(this.authPath);
  }

  async list(options?: AuthOperationOptions): Promise<readonly CredentialInfo[]> {
    options?.signal?.throwIfAborted();
    const credential = readCodexCredential(this.authPath);
    return credential ? [{ providerId: CODEX_PROVIDER_ID, type: credential.type }] : [];
  }

  /** Never persists. Returns the current credential so pi's refresh path is a no-op. */
  async modify(
    providerId: string,
    _fn: (current: Credential | undefined) => Promise<Credential | undefined>,
    options?: AuthOperationOptions,
  ): Promise<Credential | undefined> {
    options?.signal?.throwIfAborted();
    if (providerId !== CODEX_PROVIDER_ID) return undefined;
    return readCodexCredential(this.authPath);
  }

  async delete(_providerId: string, options?: AuthOperationOptions): Promise<void> {
    options?.signal?.throwIfAborted();
  }
}
