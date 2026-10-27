import { ApiError, authorizationHeaders, type ServerAuth } from "@haarness/sdk";
import type { DirectoryListing } from "../shared";
import type { WorkspaceInfo } from "@haarness/sdk";

/** Host credentials never leave Electron main. Directory APIs stay on one origin. */
async function hostRequest<T>(
  url: string,
  token: string | ServerAuth,
  path: string,
  body?: unknown,
): Promise<T> {
  const endpoint = new URL(url);
  if (
    !["http:", "https:"].includes(endpoint.protocol) ||
    endpoint.username ||
    endpoint.password ||
    endpoint.search ||
    endpoint.hash
  )
    throw new Error("Use a plain HTTP(S) server URL");
  if (
    endpoint.protocol === "http:" &&
    !["localhost", "127.0.0.1", "[::1]"].includes(endpoint.hostname)
  )
    throw new Error("Remote servers require HTTPS or a local SSH tunnel");
  const response = await fetch(new URL("/v1/host/" + path, url), {
    method: body === undefined ? "GET" : "POST",
    headers: {
      ...authorizationHeaders(token),
      "Content-Type": "application/json",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    redirect: "error",
    signal: AbortSignal.timeout(45000),
  });
  if (response.status === 404)
    throw new Error(
      "This server does not support remote project browsing yet. Update Haarness on the server; its current project remains available.",
    );
  if (!response.ok) throw await responseFailure(response);
  return response.json() as Promise<T>;
}

/** A refused request as the SDK reports one, for a request Desktop sends
 * itself: the server's code and message, or only the status when the body
 * is not Haarness's (a proxy's text or page, nothing at all). */
export async function responseFailure(response: Response): Promise<ApiError> {
  const text = await response.text().catch(() => "");
  let body: unknown = text;
  try {
    body = JSON.parse(text);
  } catch {
    /* A proxy answers in plain text or HTML. */
  }
  const outer = body && typeof body === "object" ? (body as Record<string, unknown>) : {};
  const record = outer.error && typeof outer.error === "object" ? (outer.error as Record<string, unknown>) : outer;
  return new ApiError(
    response.status,
    String(record.code ?? "http_error"),
    String(record.message ?? record.error ?? `HTTP ${response.status}`),
    record.retryable === true,
    body,
  );
}
export const browseRemote = (
  url: string,
  token: string | ServerAuth,
  path?: string,
  options: {
    prefix?: string;
    hidden?: boolean;
    limit?: number;
    after?: string;
  } = {},
) => {
  const query = new URLSearchParams();
  if (path) query.set("path", path);
  for (const [key, value] of Object.entries(options))
    if (value !== undefined) query.set(key, String(value));
  return hostRequest<DirectoryListing>(
    url,
    token,
    "directories" + (query.size ? "?" + query : ""),
  );
};
export const openRemote = (
  url: string,
  token: string | ServerAuth,
  path: string,
) => hostRequest<WorkspaceInfo>(url, token, "open", { path });
export const remoteLocationId = (url: string, workspace?: string) =>
  "remote:" +
  new URL(url).origin +
  (workspace ? "/workspaces/" + encodeURIComponent(workspace) : "");
