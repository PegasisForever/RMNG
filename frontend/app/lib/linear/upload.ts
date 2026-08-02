// Pasting an image into a ticket body, in two hops.
//
// Linear's `fileUpload` mutation answers with a Google-signed URL to PUT the bytes to, and
// the browser runs that mutation itself like every other Linear call here. What the browser
// cannot do is the PUT: the bucket answers the preflight with HTTP 200 and `vary: Origin`
// alone, no `access-control-allow-origin`, so the request never leaves. Measured twice.
//
// So the bytes take one hop through our own server, `POST /api/linear/upload-relay`. That
// route replays the PUT and nothing else. It never calls Linear's GraphQL and holds no key,
// which is what keeps the control-server out of the Linear business: the mutation, the
// signed URL, and the `assetUrl` written into the markdown all belong to this module.
//
// The signed URL carries `X-Goog-Expires=60`, so the two hops have to be back to back. There
// is no queueing or retrying an upload here for that reason.

import { gql } from "~/lib/linear/client";
import { serverErrorText } from "~/lib/serverError";

/** Where the relay lives. Same-origin, so no CORS and no key. */
const RELAY = "/api/linear/upload-relay";

const FILE_UPLOAD_MUTATION =
  "mutation($contentType: String!, $filename: String!, $size: Int!) { " +
  "fileUpload(contentType: $contentType, filename: $filename, size: $size) { " +
  "success uploadFile { uploadUrl assetUrl headers { key value } } } }";

/** One header the signed URL was computed over. Sending it back changed, or not at all, is
 *  what a 403 `SignatureDoesNotMatch` means. */
export interface UploadHeader {
  key: string;
  value: string;
}

/** What Linear hands back for one file: where to put the bytes, what to send with them, and
 *  the URL that then addresses the stored asset. */
export interface UploadTarget {
  uploadUrl: string;
  assetUrl: string;
  headers: UploadHeader[];
}

/** The headers the PUT has to carry, in the order they go out.
 *
 *  `content-type` leads because it is the one Linear signed but does not list: it is an
 *  argument to `fileUpload`, not an entry in its `headers`. Substituting anything for it,
 *  `application/octet-stream` included, is a 403. Linear's own list follows verbatim, which
 *  is where `x-goog-content-length-range` comes from, and one byte outside that range is a
 *  400 `EntityTooLarge`.
 *
 *  A duplicate of `content-type` in Linear's list is dropped rather than sent twice. */
export function putHeaders(contentType: string, signed: UploadHeader[]): UploadHeader[] {
  const headers: UploadHeader[] = [{ key: "content-type", value: contentType }];
  const seen = new Set(["content-type"]);
  for (const header of signed) {
    const key = header.key.trim().toLowerCase();
    if (key === "" || seen.has(key)) continue;
    seen.add(key);
    headers.push({ key: header.key, value: header.value });
  }
  return headers;
}

/** Linear's `fileUpload` answer, or a thrown `Error` naming what it refused. */
export function uploadTargetFromResponse(data: unknown): UploadTarget {
  const upload = (data as { fileUpload?: { success?: boolean; uploadFile?: unknown } })?.fileUpload;
  const file = upload?.success ? (upload.uploadFile as Record<string, unknown> | null) : null;
  if (!file) throw new Error("Linear refused the file upload");

  const uploadUrl = typeof file.uploadUrl === "string" ? file.uploadUrl : "";
  const assetUrl = typeof file.assetUrl === "string" ? file.assetUrl : "";
  if (uploadUrl === "") throw new Error("Linear's upload reply carried no uploadUrl");
  if (assetUrl === "") throw new Error("Linear's upload reply carried no assetUrl");

  const headers: UploadHeader[] = [];
  const listed = Array.isArray(file.headers) ? file.headers : [];
  for (const entry of listed) {
    const key = (entry as { key?: unknown })?.key;
    const value = (entry as { value?: unknown })?.value;
    if (typeof key === "string" && typeof value === "string") headers.push({ key, value });
  }
  return { uploadUrl, assetUrl, headers };
}

/** Ask Linear where to put one file's bytes. */
export async function fileUpload(
  key: string,
  file: { name: string; type: string; size: number },
): Promise<UploadTarget> {
  // A file the browser could not type is still a file worth storing. Linear signs whatever
  // string it is given, and the relay sends back the same one.
  const contentType = file.type || "application/octet-stream";
  return uploadTargetFromResponse(
    await gql<unknown>(key, FILE_UPLOAD_MUTATION, {
      contentType,
      filename: file.name,
      size: file.size,
    }),
  );
}

/** Store a pasted or dropped image in Linear and answer with its `assetUrl`.
 *
 *  This is what a ticket-body editor hands BlockNote as its `uploadFile`. The clone notes
 *  keep using `/api/upload`, which is our own store and stays LAN-local by design. */
export async function uploadToLinear(key: string, file: File): Promise<string> {
  const target = await fileUpload(key, file);
  const headers = putHeaders(file.type || "application/octet-stream", target.headers);

  const form = new FormData();
  form.append("url", target.uploadUrl);
  form.append("headers", JSON.stringify(headers));
  form.append("file", file);

  // No content-type header: the browser sets the multipart boundary itself.
  const res = await fetch(RELAY, { method: "POST", body: form });
  const body = await res.text().catch(() => "");
  if (!res.ok) {
    throw new Error(serverErrorText(body, res.statusText || `HTTP ${res.status}`));
  }
  return target.assetUrl;
}
