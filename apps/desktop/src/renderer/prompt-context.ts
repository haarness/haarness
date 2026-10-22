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
  | { kind: "agent"; name: string }
  /** A long paste kept as one chip instead of flooding the message box. */
  | { kind: "paste"; id: string; text: string };
const MAX_CONTEXT_BYTES = 128 * 1024;
/** A paste of more lines or bytes than these becomes a chip: the sizes the
 * TUI's composer folds a paste at (`input/composer.rs`). */
const PASTE_LINES = 8;
const PASTE_BYTES = 1000;
export const contextKey = (item: PromptContext) =>
  item.kind === "agent"
    ? JSON.stringify([item.kind, item.name])
    : item.kind === "paste"
      ? JSON.stringify([item.kind, item.id])
      : JSON.stringify([item.kind, item.path, item.startLine, item.endLine]);
export const contextLabel = (item: PromptContext) =>
  item.kind === "agent"
    ? "@" + item.name
    : item.kind === "paste"
      ? pasteSummary(item.text)
      : item.path +
        (item.startLine
          ? `:${item.startLine}${item.endLine !== item.startLine ? "-" + item.endLine : ""}`
          : "");
export const mentionAgents = (agents: AgentProfile[]) =>
  agents.filter((agent) => !agent.hidden && agent.mode !== "primary");
/** Lines as the TUI counts them: a final newline ends the last line rather
 * than starting another. */
export function countLines(text: string) {
  return text ? text.split("\n").length - (text.endsWith("\n") ? 1 : 0) : 0;
}
/** Pasted text with Windows and old Mac line endings made plain newlines. */
export const pastedText = (text: string) => text.replace(/\r\n?/g, "\n");
/** Whether pasting `text` (line endings already made plain) makes a chip. */
export const pasteFolds = (text: string) =>
  countLines(text) > PASTE_LINES ||
  new TextEncoder().encode(text).length > PASTE_BYTES;
/** The English summary a paste travels under; clients recount its lines. */
const pasteSummary = (text: string) => {
  const lines = Math.max(1, countLines(text));
  return `Pasted text · ${lines} ${lines === 1 ? "line" : "lines"}`;
};
/** File snapshots and delegations are bounded; a paste stands for text the
 * message box would have taken whole, so it counts toward neither limit. */
function checkContext(items: PromptContext[]) {
  const bounded = items.filter((item) => item.kind !== "paste");
  if (
    bounded.length > 20 ||
    new TextEncoder().encode(JSON.stringify(bounded)).length > MAX_CONTEXT_BYTES
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
    if (item.kind === "paste")
      return (
        typeof item.id === "string" &&
        !!item.id &&
        typeof item.text === "string" &&
        !!item.text
      );
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
/** A fence longer than any run of backticks in `content`, so the content
 * cannot close it early. */
const fenceFor = (content: string) =>
  "`".repeat(
    Array.from(content.matchAll(/`+/g)).reduce(
      (size, match) => Math.max(size, match[0].length + 1),
      3,
    ),
  );
export function promptWithContext(text: string, items: PromptContext[]) {
  checkContext(items);
  if (!items.length) return text;
  const parts = items.map((item) => {
    if (item.kind === "agent")
      return `Delegate the relevant work from this message and attached context using the task tool with agent ${JSON.stringify(item.name)}. Keep normal permissions in effect.`;
    // A paste goes whole to the model, fenced so it reads as given; the
    // block lets every client fold it under its summary.
    if (item.kind === "paste") {
      const fence = fenceFor(item.text);
      return `<details>\n<summary>${pasteSummary(item.text)}</summary>\n\n${fence}\n${item.text}\n${fence}\n\n</details>`;
    }
    const fence = fenceFor(item.content);
    return `<details>\n<summary>File context: ${escapeHtml(contextLabel(item))}${item.unsaved ? " (unsaved editor snapshot)" : " (snapshot)"}</summary>\n\n${fence}\n${item.content}\n${fence}\n\n</details>`;
  });
  return [text.trimEnd(), ...parts].filter(Boolean).join("\n\n");
}
/** One piece of a sent message: what was typed (file snapshots and
 * delegations included, which Markdown shows as written), or a paste that
 * was sent as a chip, with the text pasted. */
export type SentPart =
  | { kind: "text"; text: string }
  | { kind: "paste"; text: string };
const BLOCK_OPEN = "<details>\n<summary>";
/**
 * The context block `message` holds at `start`, as `promptWithContext` (and
 * the TUI) write them: a paste, a file snapshot or a folder listing, each
 * fenced. Its end, and for a paste the text pasted; undefined when the text
 * there only looks like one.
 */
function blockAt(message: string, start: number) {
  const from = start + BLOCK_OPEN.length;
  const close = message.indexOf("</summary>\n\n", from);
  if (close < 0) return undefined;
  const summary = message.slice(from, close);
  const paste = summary.startsWith("Pasted text");
  if (
    summary.includes("\n") ||
    !(paste || summary.startsWith("File context: ") || summary.startsWith("Directory listing: "))
  )
    return undefined;
  const body = close + "</summary>\n\n".length;
  const marker = /`{3,}(?=\n)/y;
  marker.lastIndex = body;
  const fence = marker.exec(message)?.[0];
  if (!fence) {
    // An unfenced listing, as older clients wrote one; a paste always has a fence.
    const end = paste ? -1 : message.indexOf("\n\n</details>", body);
    return end < 0 ? undefined : { end: end + "\n\n</details>".length };
  }
  // The fence is longer than any run of backticks inside, so the first
  // matching close is this block's.
  const end = message.indexOf(`\n${fence}\n\n</details>`, body + fence.length);
  if (end < 0) return undefined;
  const inner = body + fence.length + 1;
  return {
    end: end + `\n${fence}\n\n</details>`.length,
    paste: paste ? message.slice(inner, Math.max(inner, end)) : undefined,
  };
}
/** The marker a typed line opens or closes a Markdown code fence with. */
const fenceLine = (line: string) => /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line);
/**
 * A sent message split around its pastes, the inverse of `promptWithContext`
 * for them, read the way the TUI reads one (`prompt_context::parts`): a
 * context block is taken whole, so a paste-shaped block inside a file
 * snapshot stays part of that file, and nothing inside a code fence the
 * person typed counts. Text that only looks like a paste stays text.
 */
export function sentParts(message: string): SentPart[] {
  const parts: SentPart[] = [];
  let typed = 0;
  let fence: string | undefined;
  let lineStart = true;
  let at = 0;
  while (at < message.length) {
    const newline = message.indexOf("\n", at);
    const end = newline < 0 ? message.length : newline;
    const line = message.slice(at, end);
    if (lineStart) {
      const marker = fenceLine(line);
      if (fence) {
        if (marker && marker[1][0] === fence[0] && marker[1].length >= fence.length && !marker[2].trim())
          fence = undefined;
        at = end + 1;
        continue;
      }
      // A backtick fence's info string holds no backtick.
      if (marker && !(marker[1][0] === "`" && marker[2].includes("`"))) {
        fence = marker[1];
        at = end + 1;
        continue;
      }
    }
    // A block opens at the end of a line: `<details>`, then its summary.
    const start = end - "<details>".length;
    const block =
      start >= at && message.startsWith(BLOCK_OPEN, start) ? blockAt(message, start) : undefined;
    if (!block) {
      at = end + 1;
      lineStart = true;
      continue;
    }
    if (block.paste !== undefined) {
      if (typed < start) parts.push({ kind: "text", text: message.slice(typed, start) });
      parts.push({ kind: "paste", text: block.paste });
      typed = block.end;
    }
    at = block.end;
    lineStart = false;
  }
  if (typed < message.length) parts.push({ kind: "text", text: message.slice(typed) });
  return parts;
}
/** A sent message with its pastes back as the text pasted: what Copy copies. */
export const plainMessage = (message: string) =>
  sentParts(message)
    .map((part) => part.text)
    .join("");
/**
 * The prompt a slash command expanded to, when it says more than the line
 * typed (`/name arguments`): what the model received. A command that has
 * not expanded yet, or whose template is only its arguments, has nothing
 * more to show.
 */
export function commandPrompt(line: string, text: string) {
  const args = /^\/\S*\s*([\s\S]*)$/.exec(line)?.[1] ?? line;
  return text.trim() !== args.trim() ? text : undefined;
}
