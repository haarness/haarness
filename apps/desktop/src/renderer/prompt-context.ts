import type { AgentProfile } from "@haarness/sdk";

export type PromptContext =
  | {
      kind: "file";
      path: string;
      content: string;
      startLine?: number;
      endLine?: number;
      unsaved?: boolean;
    }
  | { kind: "agent"; name: string };
const MAX_CONTEXT_BYTES = 128 * 1024;
export const contextKey = (item: PromptContext) =>
  item.kind === "agent"
    ? JSON.stringify([item.kind, item.name])
    : JSON.stringify([item.kind, item.path, item.startLine, item.endLine]);
export const contextLabel = (item: PromptContext) =>
  item.kind === "agent"
    ? "@" + item.name
    : item.path +
      (item.startLine
        ? `:${item.startLine}${item.endLine !== item.startLine ? "-" + item.endLine : ""}`
        : "");
export const mentionAgents = (agents: AgentProfile[]) =>
  agents.filter((agent) => !agent.hidden && agent.mode !== "primary");
function checkContext(items: PromptContext[]) {
  if (
    items.length > 20 ||
    new TextEncoder().encode(JSON.stringify(items)).length > MAX_CONTEXT_BYTES
  )
    throw Error(
      "Context must fit within 128 KiB and 20 items. Select a smaller code range.",
    );
  return items;
}
export function readContext(value: unknown): PromptContext[] {
  if (!Array.isArray(value)) return [];
  const items = value.filter((item): item is PromptContext => {
    if (!item || typeof item !== "object") return false;
    if (item.kind === "agent")
      return typeof item.name === "string" && !!item.name;
    return (
      item.kind === "file" &&
      typeof item.path === "string" &&
      typeof item.content === "string" &&
      (item.startLine === undefined ||
        (Number.isInteger(item.startLine) &&
          item.startLine > 0 &&
          Number.isInteger(item.endLine) &&
          item.endLine >= item.startLine))
    );
  });
  try {
    return checkContext(items);
  } catch {
    return [];
  }
}
export function addContext(items: PromptContext[], item: PromptContext) {
  const key = contextKey(item);
  return checkContext([
    ...items.filter((value) => contextKey(value) !== key),
    item,
  ]);
}
export function fileContext(
  path: string,
  content: string,
  selection?: { from: number; to: number },
  unsaved = false,
): PromptContext {
  if (!selection || selection.from === selection.to)
    return { kind: "file", path, content, unsaved };
  const from = Math.min(selection.from, selection.to),
    to = Math.max(selection.from, selection.to);
  const startLine = content.slice(0, from).split("\n").length;
  // A selection ending at the next line's start does not include that line.
  const endLine = content.slice(0, Math.max(from, to - 1)).split("\n").length;
  return {
    kind: "file",
    path,
    startLine,
    endLine,
    content: content
      .split("\n")
      .slice(startLine - 1, endLine)
      .join("\n"),
    unsaved,
  };
}
export function activeMention(text: string, caret: number) {
  const prefix = text.slice(0, caret);
  const match = /(?:^|[\s(])@([^\s@]*)$/.exec(prefix);
  return match
    ? { from: caret - match[1].length - 1, to: caret, query: match[1] }
    : undefined;
}
const escapeHtml = (text: string) =>
  text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
/** Standard text requests keep context compatible with turns, queued guidance,
 * custom commands, archives and other clients. Contents are captured snapshots,
 * fenced as data and inspectable in the saved user message. */
export function promptWithContext(text: string, items: PromptContext[]) {
  checkContext(items);
  if (!items.length) return text;
  const parts = items.map((item) => {
    if (item.kind === "agent")
      return `Delegate the relevant work from this message and attached context using the task tool with agent ${JSON.stringify(item.name)}. Keep normal permissions in effect.`;
    const fence = "`".repeat(
      Array.from(item.content.matchAll(/`+/g)).reduce(
        (size, match) => Math.max(size, match[0].length + 1),
        3,
      ),
    );
    return `<details>\n<summary>File context: ${escapeHtml(contextLabel(item))}${item.unsaved ? " (unsaved editor snapshot)" : " (snapshot)"}</summary>\n\n${fence}\n${item.content}\n${fence}\n\n</details>`;
  });
  return [text.trimEnd(), ...parts].filter(Boolean).join("\n\n");
}
