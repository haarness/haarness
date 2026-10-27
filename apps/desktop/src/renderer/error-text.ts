/**
 * A failure as a person reads it: a short title when one helps, then what
 * happened. Servers name the layer an error came through ("tool error: …",
 * "invalid request: …") and pass transport text through as it was; neither
 * tells anyone what to do. Nor does an HTTP status without a sentence ("HTTP
 * 502", "Bad Gateway", a proxy's HTML page). `known` titles and messages are
 * English catalog keys; anything else is the server's own text, shown as it
 * is. A cancelled request is no failure at all: see `cancelled`.
 */
export interface ErrorText {
  title?: string;
  message: string;
  /** The text as received, for a tooltip, when the copy above replaced it. */
  detail?: string;
  known: boolean;
}

/** The same words iOS and Android use. */
const copy = {
  signIn: [
    "Sign-in could not start",
    "Another sign-in is still waiting on this computer. Finish or close it, then try again.",
  ],
  port: [
    "A port is busy",
    "Something else on this computer is using a port Haarness needs. Close it, then try again.",
  ],
  offline: ["Can't connect", "Check your internet connection and try again."],
  // Cloud relayed the request to another of your computers, which failed it.
  computer: [
    "Your computer couldn't answer",
    "Check that Haarness is open and up to date on it, then try again.",
  ],
  // Cloud has no connection to that computer now.
  unreachable: ["Your computer isn't reachable", "Open Haarness on it, then try again."],
  unavailable: ["Haarness isn't answering", "Try again in a moment."],
} as const;
/** Failures one sentence says. */
const sentences = {
  timeout: "This took too long. Check your connection, then try again.",
  server: "Something went wrong on our side. Try again in a moment.",
  slow: "This took too long. Try again in a moment.",
  busy: "Too many requests. Wait a moment, then try again.",
  forbidden: "You don't have access to this.",
  gone: "This is no longer available. It may have been removed.",
  tooLarge: "This is too large to send.",
  refused: "That didn't go through. Try again.",
} as const;
/** A titled failure on one line, as the window gets it from main and a
 * surface with room for one line shows it. */
const line = ([title, message]: readonly [string, string]) => `${title}. ${message}`;
const lines = new Map<string, readonly [string, string]>(
  Object.values(copy).map((pair) => [line(pair), pair]),
);
/** The catalog keys `errorText` and `errorLine` can return, for the
 * localization guard. */
export const errorTextKeys = [
  ...Object.values(copy).flat(),
  ...lines.keys(),
  ...Object.values(sentences),
];

const layers = /^(?:error|tool error|invalid request|storage error|internal error):\s*/i;
const busyPort = /address already in use|os error (?:48|98)\b|EADDRINUSE/i;
const offline =
  /error sending request|connection refused|ECONNREFUSED|ECONNRESET|ENOTFOUND|EAI_AGAIN|network is unreachable|dns error|failed to lookup address|^(?:fetch failed|failed to fetch|load failed|network ?error|haarness request failed)\.?$|NetworkError when attempting to fetch|ERR_(?:INTERNET_DISCONNECTED|NAME_NOT_RESOLVED|CONNECTION_(?:REFUSED|RESET|CLOSED)|TLS_CERT)|unable to verify the first certificate|self[- ]signed certificate|certificate verify failed|tls handshake/i;
const timedOut =
  /^(?:request timed out|the operation was aborted due to timeout|signal timed out|operation timed out)\.?$|error sending request\b.*\btimed out\b|\bconnect(?:ion)? timed? ?out\b|ETIMEDOUT|UND_ERR_(?:CONNECT|HEADERS|BODY)_TIMEOUT/is;
/** An address on this computer. */
const loopback = /\b(?:127(?:\.\d{1,3}){3}|localhost)\b|\[::1\]/i;
/** Cloud's words for a computer it relayed to and that failed the request. */
const computerFailed = /^your computer (?:could not complete this request|couldn['’]t answer)\b/i;
/** Cloud's words for a computer it has no connection to now. */
const remoteOffline = /^remote host is offline or its connection changed\b/i;
const aborted =
  /^(?:AbortError:\s*)?(?:(?:this|the) operation was aborted|signal is aborted without reason|the user aborted a request)\.?$/i;
const html = /^\s*<(?:!doctype|html|head|body|\?xml|title|h1)\b/i;
/** A lowercase word that starts a sentence, so it can take a capital: not a
 * model id ("gpt-5"), a path, a file name, a command or an identifier, whose
 * spelling is the point. Trailing punctuation does not count. */
const ordinaryWord = /^\p{Ll}[\p{Ll}'’]*[.,:;!?]*$/u;
/** HTTP reason phrases, which name a status and say nothing else. */
const reasons: Record<string, number> = {
  "bad request": 400,
  unauthorized: 401,
  forbidden: 403,
  "not found": 404,
  "method not allowed": 405,
  "request timeout": 408,
  conflict: 409,
  gone: 410,
  "payload too large": 413,
  "content too large": 413,
  "unprocessable entity": 422,
  "unprocessable content": 422,
  "too many requests": 429,
  "internal server error": 500,
  "not implemented": 501,
  "bad gateway": 502,
  "service unavailable": 503,
  "gateway timeout": 504,
};
/** A status line without a sentence ("HTTP 502", "502 Bad Gateway", "Bad
 * Gateway"), and the status it names; undefined for anything else. */
function statusLine(text: string): { status?: number } | undefined {
  const match = /^(?:HTTP(?:\/[\d.]+)?\s*)?(\d{3})?\s*([a-z][a-z ' -]*?)?[.!]?$/i.exec(text.trim());
  if (!match || (!match[1] && !match[2])) return undefined;
  const phrase = match[2]?.toLowerCase().replace(/time[ -]out/g, "timeout").replace(/[\s-]+/g, " ");
  if (phrase && !Object.hasOwn(reasons, phrase)) return undefined;
  return { status: match[1] ? Number(match[1]) : phrase ? reasons[phrase] : undefined };
}

/** What a failure carries besides its message: the SDK's `ApiError` (status,
 * code), its `TransportError`, DOM aborts and timeouts. Read by name, since
 * an error that crossed from main into a window keeps only its message. */
interface Failure {
  name?: unknown;
  status?: unknown;
  code?: unknown;
  body?: unknown;
  cause?: unknown;
}
const failure = (error: unknown): Failure =>
  error && typeof error === "object" ? (error as Failure) : {};
/** What the error and each of its causes say (status, name, code, message,
 * a body that was only text), as fetch nests the reason a request failed
 * ("fetch failed" ← "connect ETIMEDOUT …"). */
function causes(error: unknown) {
  const texts: string[] = [];
  for (let current = error, depth = 0; current && depth < 5; depth++) {
    const { name, status, code, body } = failure(current);
    if (typeof status === "number") texts.push(`HTTP ${status}`);
    texts.push(...[name, code].filter((value) => typeof value === "string") as string[]);
    if (current instanceof Error) texts.push(current.message);
    if (typeof body === "string" && body.trim()) texts.push(body.replace(/\s+/g, " ").trim().slice(0, 300));
    current = failure(current).cause;
  }
  return texts.join("\n");
}

/**
 * A request that was cancelled: a DOM `AbortError`, the abort of a request
 * whose window, chat or stream went away, or its message after it crossed
 * from main. It is no failure: a catch skips it and leaves what it shows as
 * it was. A `TimeoutError` (`AbortSignal.timeout`) is a timeout, not this.
 */
export function cancelled(error: unknown): boolean {
  const { name } = failure(error);
  if (name === "AbortError") return true;
  if (name === "TimeoutError" || !error) return false;
  const text = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  return aborted.test(text.trim());
}

/** The copy for a status that came without a sentence. `relay`: the request
 * went through Cloud's relay to another of your computers. */
function statusText(status: number, relay: boolean): readonly [string, string] | string {
  if (status === 502 || status === 503 || status === 504) return relay ? copy.computer : copy.unavailable;
  if (status >= 500) return sentences.server;
  if (status === 408) return sentences.slow;
  if (status === 429) return sentences.busy;
  if (status === 403) return sentences.forbidden;
  if (status === 404 || status === 410) return sentences.gone;
  if (status === 413) return sentences.tooLarge;
  return sentences.refused;
}

export function errorText(error: unknown, options: { relay?: boolean } = {}): ErrorText {
  // A catch that shows it anyway shows nothing.
  if (cancelled(error)) return { message: "", known: true };
  let text = (error instanceof Error ? error.message : String(error ?? "")).trim();
  while (layers.test(text)) text = text.replace(layers, "").trim();
  const sentence = (value: string) =>
    ordinaryWord.test(value.split(/\s/, 1)[0])
      ? value.charAt(0).toUpperCase() + value.slice(1)
      : value;
  const known = (chosen: readonly [string, string] | string): ErrorText => {
    const [title, message] = typeof chosen === "string" ? [undefined, chosen] : chosen;
    const said = title ? line([title, message]) : message;
    return {
      ...(title ? { title } : {}),
      message,
      ...(text && text !== said ? { detail: text } : {}),
      known: true,
    };
  };
  // Copy chosen before, as main hands it to the window.
  const chosen = lines.get(text);
  if (chosen) return known(chosen);
  const { name, status: raw, code } = failure(error);
  const status = typeof raw === "number" ? raw : statusLine(text)?.status;
  if (code === "bad_gateway" || code === "project_server_unavailable" || computerFailed.test(text))
    return known(copy.computer);
  if (
    code === "remote_offline" ||
    remoteOffline.test(text) ||
    (options.relay && status === 409 && /offline|connection changed/i.test(text))
  )
    return known(copy.unreachable);
  // A stream that went silent timed out too.
  const transport = name === "TransportError" ? causes(error) : "";
  if (name === "TimeoutError" || timedOut.test(text) || timedOut.test(transport) || /sent nothing for/.test(transport))
    return known(sentences.timeout);
  if (busyPort.test(text))
    return known(/sign-?in|login|1455/i.test(text) ? copy.signIn : copy.port);
  // A body that broke off or was not Haarness's, or no connection at all. A
  // server on this computer that takes none is Haarness not answering, not
  // the internet.
  if (transport)
    return known(/invalid|no body|ended before|exceeds/i.test(text) || loopback.test(transport) ? copy.unavailable : copy.offline);
  if (offline.test(text)) return known(loopback.test(text) ? copy.unavailable : copy.offline);
  // A status without a sentence, or a proxy's page in place of one.
  if ((status && status >= 400 && !text) || statusLine(text))
    return known(statusText(status ?? 0, !!options.relay));
  if (html.test(text))
    return known(status && status >= 400 ? statusText(status, !!options.relay) : copy.unavailable);
  // "OpenAI sign-in failed: another sign-in is using port 1455 …" reads
  // better as a title over the reason.
  const failed = /^([^:\n]{3,60} failed):\s*(.+)$/s.exec(text);
  if (failed)
    return { title: failed[1], message: sentence(failed[2]), known: false };
  return { message: sentence(text), known: false };
}

/** A failure on one line, for a surface without room for a title: an English
 * catalog key when `known`, as `errorText` says. */
export const errorTextLine = (text: ErrorText) =>
  text.title ? (text.known ? line([text.title, text.message]) : `${text.title}: ${text.message}`) : text.message;

/**
 * The words main hands a window for a failure: the window gets only an
 * error's message, so copy that needs what the message leaves out (an
 * `ApiError`'s status and code, a `TransportError`, a timeout, the relay)
 * is chosen here, on one line. A message the window would read the same way
 * goes as it is, so its tooltip keeps it. A cancellation stays one.
 */
export function failureMessage(error: unknown, options: { relay?: boolean } = {}): string {
  const message = error instanceof Error ? error.message : String(error ?? "");
  if (cancelled(error)) return "This operation was aborted";
  const full = errorText(error, options);
  return full.known && errorTextLine(full) !== errorTextLine(errorText(message))
    ? errorTextLine(full)
    : message;
}

/** A failure as it was received, on one line for a log: the status, name,
 * code and message of it and of each cause. */
export const failureDetail = (error: unknown) =>
  (causes(error) || String(error ?? "")).split("\n").join(" · ");
