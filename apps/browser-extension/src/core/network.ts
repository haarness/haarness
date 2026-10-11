/**
 * Chrome-only tightening of `network.get` results from the shared driver. In
 * the user's own browser, requests carry the user's credentials: request
 * bodies (login forms, tokens) are never returned, and headers that commonly
 * hold secrets are redacted on top of the driver's own redaction.
 */

const SECRET_HEADER = /^(?:cookie|set-cookie|authorization|proxy-authorization|x-api-key|x-[\w-]*-token|x-csrf-[\w-]*|x-xsrf-[\w-]*)$/i;

interface Header {
  name: string;
  value: string;
}

export function chromeNetworkResult(value: unknown): unknown {
  if (!value || typeof value !== "object") return value;
  const result = value as Record<string, unknown>;
  const redact = (headers: unknown) =>
    Array.isArray(headers)
      ? (headers as Header[]).map((header) => (SECRET_HEADER.test(header.name) ? { ...header, value: "[redacted]" } : header))
      : headers;
  return {
    ...result,
    requestHeaders: redact(result.requestHeaders),
    responseHeaders: redact(result.responseHeaders),
    // Omitted in Chrome: a request body may be the user's password or token.
    ...(result.requestBody ? { requestBody: { state: "unavailable", reason: "notCaptured" } } : {}),
  };
}
