/**
 * A failure as a person reads it: a short title when one helps, then what
 * happened. Servers name the layer an error came through ("tool error: …",
 * "invalid request: …") and pass transport text through as it was; neither
 * tells anyone what to do. `known` titles and messages are English catalog
 * keys; anything else is the server's own text, shown as it is.
 */
export interface ErrorText {
  title?: string;
  message: string;
  /** The text as received, for a tooltip, when the copy above replaced it. */
  detail?: string;
  known: boolean;
}

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
} as const;
/** The catalog keys `errorText` can return, for the localization guard. */
export const errorTextKeys = Object.values(copy).flat();

const layers = /^(?:error|tool error|invalid request|storage error|internal error):\s*/i;
const busyPort = /address already in use|os error (?:48|98)\b|EADDRINUSE/i;
const offline =
  /error sending request|connection refused|ECONNREFUSED|ECONNRESET|ENOTFOUND|network is unreachable|dns error|failed to lookup address|connect timed? ?out/i;
/** A lowercase word that starts a sentence, so it can take a capital: not a
 * model id ("gpt-5"), a path, a file name, a command or an identifier, whose
 * spelling is the point. Trailing punctuation does not count. */
const ordinaryWord = /^\p{Ll}[\p{Ll}'’]*[.,:;!?]*$/u;

export function errorText(error: unknown): ErrorText {
  let text = (error instanceof Error ? error.message : String(error ?? "")).trim();
  while (layers.test(text)) text = text.replace(layers, "").trim();
  const sentence = (value: string) =>
    ordinaryWord.test(value.split(/\s/, 1)[0])
      ? value.charAt(0).toUpperCase() + value.slice(1)
      : value;
  const known = ([title, message]: readonly [string, string]) => ({
    title,
    message,
    detail: text,
    known: true,
  });
  if (busyPort.test(text))
    return known(/sign-?in|login|1455/i.test(text) ? copy.signIn : copy.port);
  if (offline.test(text)) return known(copy.offline);
  // "OpenAI sign-in failed: another sign-in is using port 1455 …" reads
  // better as a title over the reason.
  const failed = /^([^:\n]{3,60} failed):\s*(.+)$/s.exec(text);
  if (failed)
    return { title: failed[1], message: sentence(failed[2]), known: false };
  return { message: sentence(text), known: false };
}
