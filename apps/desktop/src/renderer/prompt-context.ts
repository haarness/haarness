import type { AgentProfile } from "@haarness/sdk";
import {
  elementKey,
  elementLabel,
  elementPrompt,
  fence,
  isElementContext,
  type ElementContext,
} from "./element-context";

export type PromptContext =
  | {
      kind: "file";
      path: string;
      content: string;
      startLine?: number;
      endLine?: number;
      unsaved?: boolean;
    }
  | { kind: "agent"; name: string }
  | ElementContext;
const MAX_CONTEXT_BYTES = 128 * 1024;
export const contextKey = (item: PromptContext) =>
  item.kind === "agent"
    ? JSON.stringify([item.kind, item.name])
    : item.kind === "element"
      ? elementKey(item)
      : JSON.stringify([item.kind, item.path, item.startLine, item.endLine]);
export const contextLabel = (item: PromptContext) =>
  item.kind === "agent"
    ? "@" + item.name
    : item.kind === "element"
      ? elementLabel(item)
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
    if (item.kind === "element") return isElementContext(item);
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
  return fileLinesContext(path, content, startLine, endLine, unsaved);
}
/** Lines `startLine` to `endLine` (from 1, both included) of a file, as
 * context: a selection in a Markdown file's Preview names its lines. */
export function fileLinesContext(
  path: string,
  content: string,
  startLine: number,
  endLine: number,
  unsaved = false,
): PromptContext {
  const lines = content.split("\n");
  const first = Math.max(1, Math.min(startLine, lines.length));
  const last = Math.max(first, Math.min(endLine, lines.length));
  return {
    kind: "file",
    path,
    startLine: first,
    endLine: last,
    content: lines.slice(first - 1, last).join("\n"),
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
    if (item.kind === "element") return elementPrompt(item, escapeHtml);
    const marks = fence(item.content);
    return `<details>\n<summary>File context: ${escapeHtml(contextLabel(item))}${item.unsaved ? " (unsaved editor snapshot)" : " (snapshot)"}</summary>\n\n${marks}\n${item.content}\n${marks}\n\n</details>`;
  });
  return [text.trimEnd(), ...parts].filter(Boolean).join("\n\n");
}
