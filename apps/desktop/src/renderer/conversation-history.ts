import type { Item, Thread, UnrestoredFile } from "@haarness/sdk";
import { plainMessage } from "./prompt-context";
import { spelledOutPlain, withoutEscapes } from "./display-text";
import { t } from "./i18n";

export function retainedItems(items: Item[], thread?: Thread) {
  const boundary = thread?.revert?.sequence ?? Infinity;
  return items.filter((item) => item.sequence <= boundary);
}
/** The files a pending undo left as they are, as the server named them:
 * those left for their size, which the folder no longer matches a
 * checkpoint at, apart from the ignored and outside ones, and how many
 * more there are than it named. An older server names none. */
export function unrestoredFiles(thread?: Thread) {
  const files = thread?.revert?.unrestored ?? [];
  const count = Math.max(files.length, thread?.revert?.unrestoredCount ?? 0);
  return {
    large: files.filter((file) => file.reason === "large"),
    quiet: files.filter((file) => file.reason !== "large"),
    more: count - files.length,
    count,
  };
}
/** Why an undo left `file` as it was, in the interface language; nothing
 * for a reason only a newer server knows. */
export function unrestoredReason(file: UnrestoredFile) {
  switch (file.reason) {
    case "large":
      return t("too large for Undo");
    case "ignored":
      return t("ignored");
    case "outside":
      return t("outside the folder");
    default:
      return undefined;
  }
}
export function lastUserMessage(items: Item[], boundary = Infinity) {
  return items.findLast(
    (item) =>
      item.sequence <= boundary &&
      item.data.type === "user_message" &&
      !item.data.synthetic &&
      !item.data.queued,
  );
}

/** Search readable content, not IDs, snapshots, or provider metadata. Results
 * are indexed from history so unmounted virtual rows remain searchable. */
function conversationText(item: Item, items: Item[]) {
  const data = item.data;
  switch (data.type) {
    // What the bubble shows: the line a slash command was typed as, and
    // pasted text without the block it was sent in.
    case "user_message":
      return data.synthetic ? "" : plainMessage(data.command_line || data.text);
    case "assistant_message":
    case "reasoning":
      return data.text;
    case "plan":
      return data.markdown;
    case "compaction":
      return data.summary;
    case "error":
      return data.message;
    case "review":
      return data.findings
        .map((finding) => JSON.stringify(finding))
        .join("\n");
    case "tool_call": {
      const result = items.find(
        (value) =>
          value.turnId === item.turnId &&
          value.sequence > item.sequence &&
          value.data.type === "tool_result" &&
          value.data.call_id === data.call_id,
      );
      return [
        data.name,
        // With what would not show spelled out, so the preview cannot be
        // reordered and searching for a code finds it.
        spelledOutPlain(JSON.stringify(data.arguments)),
        // As the row shows it, without its escape sequences.
        result?.data.type === "tool_result" ? withoutEscapes(result.data.output) : "",
      ].join("\n");
    }
    default:
      return "";
  }
}
export function findConversationMessages(
  rows: Item[],
  items: Item[],
  query: string,
) {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [];
  return rows.flatMap((item) => {
    const text = conversationText(item, items);
    const start = text.toLocaleLowerCase().indexOf(needle);
    if (start < 0) return [];
    return [
      {
        id: item.id,
        before: text.slice(Math.max(0, start - 45), start),
        match: text.slice(start, start + needle.length),
        after: text.slice(start + needle.length, start + needle.length + 90),
      },
    ];
  });
}
