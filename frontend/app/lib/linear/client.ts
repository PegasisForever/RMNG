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
    });
  } catch (e) {
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
