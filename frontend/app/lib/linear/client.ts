// One POST to Linear's GraphQL endpoint, and what the operator is told when it fails.
//
// The browser talks to `api.linear.app` directly: the preflight there echoes any origin back
// and allows the `authorization` header, so no proxy of ours sits in the middle. Auth is the
// preset's personal API key verbatim, with no `Bearer` prefix, which is the one shape Linear
// accepts for a personal key.
//
// Streaming is not an option from a page. Linear's GraphQL subscriptions authenticate off an
// HTTP header on the WebSocket handshake, which a browser cannot set; every payload-based
// shape is acked and then closed with 4002. So every reader here polls.

import { serverErrorText } from "~/lib/serverError";

const LINEAR_API = "https://api.linear.app/graphql";

/** How long one request may hang before it is abandoned.
 *
 *  A `fetch` with no signal has no deadline of its own: a half-open socket is only given up
 *  on when the OS times the connection out, which is minutes. Every caller here runs on a
 *  timer, so a request that outlives its own interval is worth nothing even if it eventually
 *  answers, and holding the slot is worse than losing the answer.
 *
 *  20 seconds is the balance. It is far past any answer this API has been observed to give
 *  for the queries in `queries.ts` and the mutations in `mutations.ts`, so a merely slow
 *  network is not cut off, and it is well inside the 60-second poll so one wedged socket
 *  costs a single round rather than every round after it. */
const TIMEOUT_MS = 20_000;

/** One GraphQL request, or a thrown `Error` carrying the sentence a banner can show.
 *
 *  A 200 is not success. GraphQL answers a rejected query with HTTP 200 and an `errors`
 *  array, so the body decides the outcome and the status only breaks ties. The first error's
 *  `message` is the one shown: Linear sends one entry per problem and the rest repeat it for
 *  the other fields of the same selection.
 *
 *  The body is read once as text and parsed here rather than through `res.json()`, the same
 *  discipline `request` in `~/lib/api` uses, because a failure that never reached Linear
 *  arrives as something else entirely: a proxy's HTML page, or a plain-text rate-limit line.
 *  `serverErrorText` is what picks a banner sentence out of those, and it is used only for
 *  them. It cannot read the GraphQL `errors` shape, which is neither of the two bodies it
 *  knows about, so that case is handled above it. */
export async function gql<T>(
  key: string,
  query: string,
  variables: Record<string, unknown> = {},
): Promise<T> {
  if (key.trim() === "") throw new Error("no Linear API key configured for that workspace");

  let res: Response;
  try {
    res = await fetch(LINEAR_API, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: key },
      body: JSON.stringify({ query, variables }),
      signal: AbortSignal.timeout(TIMEOUT_MS),
    });
  } catch (e) {
    // An abort is our own deadline, not the network refusing. Saying so names the thing the
    // operator can act on, which is a connection that is up but not delivering.
    if ((e as Error).name === "TimeoutError") {
      throw new Error(`Linear API did not answer within ${TIMEOUT_MS / 1000}s`);
    }
    throw new Error(`Linear API unreachable: ${(e as Error).message}`);
  }

  const body = await res.text().catch(() => "");
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    parsed = null;
  }

  const message = firstErrorMessage(parsed);
  if (message !== null) throw new Error(message);
  if (!res.ok) throw new Error(serverErrorText(body, `Linear API error (HTTP ${res.status})`));
  if (parsed === null) throw new Error("Linear API sent a body that is not JSON");

  const data = (parsed as { data?: unknown }).data;
  if (data === undefined || data === null) throw new Error("Linear API returned no data");
  return data as T;
}

/** The first `errors[].message` in a GraphQL response body, or null when it holds none. */
function firstErrorMessage(parsed: unknown): string | null {
  if (typeof parsed !== "object" || parsed === null) return null;
  const errors = (parsed as { errors?: unknown }).errors;
  if (!Array.isArray(errors)) return null;
  for (const entry of errors) {
    const message = (entry as { message?: unknown })?.message;
    if (typeof message === "string" && message.trim() !== "") return message;
  }
  return null;
}
