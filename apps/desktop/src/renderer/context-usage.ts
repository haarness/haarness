import type { Item, ItemData, Thread } from "@haarness/sdk";
import { t } from "./i18n";

export const contextKinds = [
  "User messages",
  "Assistant messages",
  "Tool calls",
  "Tool results",
  "Compaction summary",
] as const;

/** How the hidden note a left-out turn that changed things leaves begins. */
const LEFT_OUT_NOTE = "<left-out-turn>\n";

/** Whether a message with no turn of its own (a fork's copy) opened one: the
 * person's, or one Haarness sent to start a turn, not a reminder, a note, a
 * Stop hook's feedback or steering. As the runtime groups them. */
function opensTurn(data: ItemData) {
  if (data.type !== "user_message" || data.queued) return false;
  if (!data.synthetic) return true;
  return !data.hook && !data.text.startsWith("<system-reminder>") && !data.text.startsWith(LEFT_OUT_NOTE);
}

/** The ids of the items of every turn a safety stop left out of later
 * requests (an error with `turn_left_out`), except a compaction made during
 * it and its note of what it changed: what the runtime leaves out. */
export function leftOut(items: Item[]) {
  const left = new Set<string>();
  if (!items.some((item) => item.data.type === "error" && item.data.turn_left_out)) return left;
  let copied: string | null = null;
  const turns = items.map((item) => {
    if (item.turnId) {
      copied = null;
      return item.turnId;
    }
    if (opensTurn(item.data)) copied = "copied:" + item.id;
    return copied;
  });
  const stopped = new Set(
    items.flatMap((item, index) =>
      item.data.type === "error" && item.data.turn_left_out && turns[index] ? [turns[index]] : [],
    ),
  );
  items.forEach((item, index) => {
    const data = item.data;
    const note = data.type === "user_message" && !!data.synthetic && data.text.startsWith(LEFT_OUT_NOTE);
    if (turns[index] && stopped.has(turns[index]) && data.type !== "compaction" && !note) left.add(item.id);
  });
  return left;
}

/** Saved text still retained after undo and the latest compaction. Mirrors the
 * runtime's summary + retained tail boundary, and the turns it leaves out,
 * without pretending to tokenize provider-specific system prompts, schemas
 * or media. */
export function contextUsage(items: Item[], thread: Thread) {
  const history = items.filter((item) => item.threadId === thread.id);
  const undone = history.filter(
    (item) => item.sequence <= (thread.revert?.sequence ?? Infinity),
  );
  const left = leftOut(undone);
  const retained = undone.filter((item) => !left.has(item.id));
  const summary = retained.findLast(
    (item) => item.data.type === "compaction",
  );
  const compaction = summary?.data.type === "compaction" ? summary.data : null;
  const tail = compaction?.tail_start_sequence;
  // Items the summary kept ahead of its tail: the latest request.
  const kept = new Set(compaction?.kept_sequences ?? []);
  // The most characters a tool result kept from before the summary shows.
  const outputChars = compaction?.tool_output_chars ?? null;
  const visible = summary
    ? retained.filter(
        (item) =>
          item.id === summary.id ||
          item.sequence > summary.sequence ||
          (item.data.type !== "compaction" &&
            ((tail != null && item.sequence >= tail) ||
              kept.has(item.sequence))),
      )
    : retained;
  const counts = new Map<string, number>(
    contextKinds.map((kind) => [kind, 0]),
  );
  let attachments = 0;
  for (const item of visible) {
    const data = item.data;
    let kind: string, text: string;
    switch (data.type) {
      case "user_message":
        kind = "User messages";
        text = data.text;
        attachments += data.attachments?.length ?? 0;
        break;
      case "assistant_message":
        // A note goes back to the model only as the thinking block it
        // came in, which this estimate cannot see.
        if (data.note) continue;
        kind = "Assistant messages";
        text = data.text;
        break;
      case "tool_call":
        kind = "Tool calls";
        text = data.name + JSON.stringify(data.arguments);
        break;
      case "tool_result":
        kind = "Tool results";
        text =
          outputChars != null && summary && item.sequence < summary.sequence
            ? Array.from(data.output).slice(0, outputChars).join("")
            : data.output;
        break;
      case "compaction":
        kind = "Compaction summary";
        text =
          data.summary +
          data.files_touched.join("\n") +
          data.outstanding.join("\n") +
          (data.todo_checkpoint ? JSON.stringify(data.todo_checkpoint) : "");
        break;
      default:
        continue;
    }
    // Code points avoid counting surrogate pairs twice. This is a heuristic,
    // not the provider tokenizer; accuracy varies with language and content.
    counts.set(
      kind,
      counts.get(kind)! + Math.ceil(Array.from(text).length / 4),
    );
  }
  const breakdown = contextKinds.map((label) => ({
    label,
    tokens: counts.get(label)!,
  }));
  return {
    breakdown,
    total: breakdown.reduce((sum, row) => sum + row.tokens, 0),
    attachments,
    latest: retained.findLast(
      (item) =>
        item.data.type === "usage" &&
        (!summary || item.sequence > summary.sequence),
    ),
    reports: history.filter((item) => item.data.type === "usage").length,
    messages: visible.filter(
      (item) =>
        ["user_message", "assistant_message"].includes(item.data.type) &&
        !(item.data.type === "assistant_message" && item.data.note),
    ).length,
    compacted: !!summary,
  };
}

/** A token count in the reader's language ("1,234", "1.234"), or that it is not known. */
export const tokenNumber = (value: number | null | undefined, locale: string) =>
  value == null ? t("Not available") : new Intl.NumberFormat(locale).format(value);

/** A share of a limit for a "{percent}%" label, to one decimal ("12.5", "12,5"). */
export const percentNumber = (value: number, locale: string) =>
  new Intl.NumberFormat(locale, { minimumFractionDigits: 1, maximumFractionDigits: 1 }).format(value);

/** A cost in dollars to four decimals, written as the reader's language writes money. */
export const costNumber = (usd: number, locale: string) =>
  new Intl.NumberFormat(locale, {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 4,
    maximumFractionDigits: 4,
  }).format(usd);

/** A request's pace from its usage record: the wait for its first streamed
 * content, and the tokens that streamed per second while they streamed (not
 * the provider's output count, which also holds reasoning it never streamed).
 * Speed is left out under half a second of streaming, where one burst reads
 * as an absurd rate, and for a request that streamed nothing. Records of
 * stopped or failed requests, and those made before requests were timed,
 * carry no timing. */
export function requestPace(data: ItemData | undefined) {
  if (data?.type !== "usage" || data.first_token_ms == null) return undefined;
  const stream = data.stream_ms ?? 0;
  const streamed = data.stream_tokens ?? data.output_tokens;
  return {
    firstTokenMs: data.first_token_ms,
    tokensPerSecond:
      stream >= 500 && streamed > 0 ? streamed / (stream / 1000) : undefined,
  };
}

/** One decimal under ten, whole numbers from there, rounded to tenths first so
 * 9.96 reads 10 rather than 10.0. */
const paceDigits = (value: number) => (Math.round(value * 10) < 100 ? 1 : 0);

/** A first-token time in seconds, in the reader's language ("1.2 s", "1,2 s"). */
export const paceSeconds = (ms: number, locale: string) =>
  new Intl.NumberFormat(locale, {
    style: "unit",
    unit: "second",
    unitDisplay: "narrow",
    minimumFractionDigits: paceDigits(ms / 1000),
    maximumFractionDigits: paceDigits(ms / 1000),
  }).format(ms / 1000);

/** A tokens-per-second figure, for the "{count} tokens/s" label. */
export const paceSpeed = (tokensPerSecond: number, locale: string) =>
  new Intl.NumberFormat(locale, {
    minimumFractionDigits: paceDigits(tokensPerSecond),
    maximumFractionDigits: paceDigits(tokensPerSecond),
  }).format(tokensPerSecond);
