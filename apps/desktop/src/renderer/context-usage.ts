import type { Item, ItemData, Thread } from "@haarness/sdk";

export const contextKinds = [
  "User messages",
  "Assistant messages",
  "Tool calls",
  "Tool results",
  "Compaction summary",
] as const;

/** Saved text still retained after undo and the latest compaction. Mirrors the
 * runtime's summary + retained tail boundary, without pretending to tokenize
 * provider-specific system prompts, schemas or media. */
export function contextUsage(items: Item[], thread: Thread) {
  const history = items.filter((item) => item.threadId === thread.id);
  const retained = history.filter(
    (item) => item.sequence <= (thread.revert?.sequence ?? Infinity),
  );
  const summary = retained.findLast(
    (item) => item.data.type === "compaction",
  );
  const tail =
    summary?.data.type === "compaction"
      ? summary.data.tail_start_sequence
      : null;
  const visible = summary
    ? retained.filter(
        (item) =>
          item.id === summary.id ||
          item.sequence > summary.sequence ||
          (tail != null &&
            item.sequence >= tail &&
            item.data.type !== "compaction"),
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
        kind = "Assistant messages";
        text = data.text;
        break;
      case "tool_call":
        kind = "Tool calls";
        text = data.name + JSON.stringify(data.arguments);
        break;
      case "tool_result":
        kind = "Tool results";
        text = data.output;
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
    messages: visible.filter((item) =>
      ["user_message", "assistant_message"].includes(item.data.type),
    ).length,
    compacted: !!summary,
  };
}

export const tokenNumber = (value: number | null | undefined) =>
  value == null ? "Not available" : value.toLocaleString("en-US");

/** A request's pace from its usage record: the wait for its first streamed
 * content, and output tokens per second while it streamed. Speed is left out
 * under half a second of streaming, where one burst reads as an absurd rate,
 * and for a request that produced nothing. Records of stopped or failed
 * requests, and those made before requests were timed, carry no timing. */
export function requestPace(data: ItemData | undefined) {
  if (data?.type !== "usage" || data.first_token_ms == null) return undefined;
  const stream = data.stream_ms ?? 0;
  return {
    firstTokenMs: data.first_token_ms,
    tokensPerSecond:
      stream >= 500 && data.output_tokens > 0
        ? data.output_tokens / (stream / 1000)
        : undefined,
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
