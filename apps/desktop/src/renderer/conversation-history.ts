import type { Item, Thread } from "@haarness/sdk";

export function retainedItems(items: Item[], thread?: Thread) {
  const boundary = thread?.revert?.sequence ?? Infinity;
  return items.filter((item) => item.sequence <= boundary);
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
export function conversationText(item: Item, items: Item[]) {
  const data = item.data;
  switch (data.type) {
    case "user_message":
      return data.synthetic ? "" : data.text;
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
        JSON.stringify(data.arguments),
        result?.data.type === "tool_result" ? result.data.output : "",
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
