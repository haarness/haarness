import type * as T from "@haarness/sdk";

/** What a hook said, for the quiet note that stands for the message a hook
 * kept the turn going with: its event and the text of its feedback block.
 * Undefined for any other item, which shows as it always did. */
export function hookNote(data: T.ItemData): { event: string; text: string } | undefined {
  if (data.type !== "user_message" || !data.hook) return undefined;
  return { event: data.hook, text: hookFeedback(data.text) };
}

/** The text inside a message's `<hook-feedback …>` block, or all of it when
 * it has none (as the server's `hook_feedback` reads it). */
export function hookFeedback(text: string): string {
  const start = text.indexOf("<hook-feedback");
  const open = start < 0 ? -1 : text.indexOf(">", start);
  const close = text.lastIndexOf("</hook-feedback>");
  return (open >= 0 && close > open ? text.slice(open + 1, close) : text).trim();
}
