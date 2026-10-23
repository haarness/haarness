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
  | ElementContext
  /** A long paste kept as one chip instead of flooding the message box. */
  | { kind: "paste"; id: string; text: string }
  | ResourceContext;
/** An MCP resource picked from the `@` list. Only its address travels with
 * the message; the server reads it from `server` when the message is sent.
 * One that Undo or a queued message's edit brings back holds `block`, what
 * the server read for it as the message carried it, and is sent as it was,
 * without reading it again, as a paste is. */
export type ResourceContext = {
  kind: "resource";
  server: string;
  uri: string;
  name: string;
  description?: string;
  mimeType?: string;
  block?: string;
};
/** What a server must say before Desktop sends it resource mentions: an
 * older one would drop them without a word. */
export const RESOURCE_MENTIONS = "mcp-resource-mentions.v1";
const MAX_CONTEXT_BYTES = 128 * 1024;
/** Enough for a page's worth of picked elements; the byte bound is the real limit. */
const MAX_CONTEXT_ITEMS = 50;
/** A paste of more lines or bytes than these becomes a chip: the sizes the
 * TUI's composer folds a paste at (`input/composer.rs`). */
const PASTE_LINES = 8;
const PASTE_BYTES = 1000;
export const contextKey = (item: PromptContext) =>
  item.kind === "agent"
    ? JSON.stringify([item.kind, item.name])
    : item.kind === "element"
      ? elementKey(item)
      : item.kind === "paste"
        ? JSON.stringify([item.kind, item.id])
        : item.kind === "resource"
          ? // Two servers may expose one URI: each is its own mention.
            JSON.stringify([item.kind, item.server, item.uri])
          : JSON.stringify([item.kind, item.path, item.startLine, item.endLine]);
export const contextLabel = (item: PromptContext) =>
  item.kind === "agent" || item.kind === "resource"
    ? "@" + item.name
    : item.kind === "element"
      ? elementLabel(item)
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
/** What the desktop keeps in one stored value (`bounded` in the main
 * process), counted as it counts it. */
const STORED_LIMIT = 2 * 1024 * 1024;
/**
 * The draft's context as it can be kept across a reload: everything, unless
 * pastes push it past what Desktop stores in one value; then the pastes that
 * do not fit are left out (they stay in the open draft and are sent), and
 * `dropped` names them.
 */
export function keptContext(items: PromptContext[]) {
  if (JSON.stringify(items).length <= STORED_LIMIT) return { kept: items, dropped: [] as string[] };
  const dropped: string[] = [];
  let kept: PromptContext[] = items.filter((item) => item.kind !== "paste");
  for (const item of items) {
    if (item.kind !== "paste") continue;
    const next = items.filter((value) => kept.includes(value) || value === item);
    if (JSON.stringify(next).length <= STORED_LIMIT) kept = next;
    else dropped.push(item.id);
  }
  return { kept, dropped };
}
/** How big a paste is, said only once it is big enough to matter: "2.4 MB". */
export function pasteSize(text: string, locale: string) {
  const bytes = new TextEncoder().encode(text).length;
  if (bytes < 100 * 1000) return "";
  const [unit, value] = bytes < 1000 * 1000 ? (["kilobyte", bytes / 1000] as const) : (["megabyte", bytes / 1000 / 1000] as const);
  return new Intl.NumberFormat(locale, {
    style: "unit",
    unit,
    unitDisplay: "short",
    maximumFractionDigits: unit === "megabyte" ? 1 : 0,
  }).format(value);
}
/** What was already sent: a paste, or a resource as the server read it. */
const carried = (item: PromptContext) => item.kind === "paste" || (item.kind === "resource" && item.block !== undefined);
/** File snapshots, delegations, picked elements and resources to read are
 * bounded; a paste stands for text the message box would have taken whole,
 * and a resource brought back holds what was sent before, so neither counts
 * toward a limit. */
function checkContext(items: PromptContext[]) {
  const bounded = items.filter((item) => !carried(item));
  if (bounded.length > MAX_CONTEXT_ITEMS)
    throw Error(
      `A message holds up to ${MAX_CONTEXT_ITEMS} context items. Remove some before adding more.`,
    );
  if (new TextEncoder().encode(JSON.stringify(bounded)).length > MAX_CONTEXT_BYTES)
    throw Error(
      "Context must fit within 128 KiB. Select a smaller code range, or remove some context.",
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
    if (item.kind === "resource")
      return (
        [item.server, item.uri, item.name].every((value) => typeof value === "string" && !!value) &&
        [item.description, item.mimeType, item.block].every((value) => value === undefined || typeof value === "string")
      );
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
export function promptWithContext(text: string, items: PromptContext[]) {
  checkContext(items);
  // Resources to read travel as mentions (`resourceMentions`); one brought
  // back is written as it was sent.
  const written = items.filter((item) => item.kind !== "resource" || item.block !== undefined);
  if (!written.length) return text;
  const parts = written.map((item) => {
    if (item.kind === "resource") return item.block!;
    if (item.kind === "agent")
      return `Delegate the relevant work from this message and attached context using the task tool with agent ${JSON.stringify(item.name)}. Keep normal permissions in effect.`;
    if (item.kind === "element") return elementPrompt(item, escapeHtml);
    // A paste goes whole to the model, fenced so it reads as given; the
    // block lets every client fold it under its summary.
    if (item.kind === "paste") {
      const marks = fence(item.text);
      return `<details>\n<summary>${pasteSummary(item.text)}</summary>\n\n${marks}\n${item.text}\n${marks}\n\n</details>`;
    }
    const marks = fence(item.content);
    return `<details>\n<summary>File context: ${escapeHtml(contextLabel(item))}${item.unsaved ? " (unsaved editor snapshot)" : " (snapshot)"}</summary>\n\n${marks}\n${item.content}\n${marks}\n\n</details>`;
  });
  return [text.trimEnd(), ...parts].filter(Boolean).join("\n\n");
}
/** The resources a draft mentions, as the server reads them at send. */
export const resourceMentions = (items: PromptContext[]) =>
  items.flatMap((item) =>
    item.kind === "resource" && item.block === undefined
      ? [{ server: item.server, uri: item.uri, name: item.name }]
      : [],
  );
/** What a sent or queued message's resource comes back as: a chip holding
 * the block as it was sent. Its address is `<server>:<uri>`; a server name
 * has no colon of its own in practice (its tools' names could not hold one). */
export function carriedResource(part: Extract<SentPart, { kind: "resource" }>): ResourceContext {
  return { ...resourceToRead(part), block: part.text };
}
/** A sent message's resource as a chip to read again when sent: what Undo
 * brings back for one that could not be read. */
export function resourceToRead(part: Extract<SentPart, { kind: "resource" }>): ResourceContext {
  const colon = part.address.indexOf(":");
  const server = colon < 0 ? part.address : part.address.slice(0, colon);
  const uri = colon < 0 ? part.address : part.address.slice(colon + 1);
  return { kind: "resource", server, uri, name: part.name === part.address ? uri : part.name };
}
/** One piece of a sent message: what was typed (file snapshots, picked
 * page elements and delegations included, which Markdown shows as written),
 * a paste that was sent as a chip, with the text pasted, or what the server
 * read for a resource mentioned with `@`: `text` is its whole block as sent,
 * `content` what was read (or why it was not), `name` and `address` what
 * its label says. */
export type SentPart =
  | { kind: "text"; text: string }
  | { kind: "paste"; text: string }
  | { kind: "resource"; text: string; name: string; address: string; content: string; failed: boolean };
const RESOURCE = "MCP resource: ";
const NOT_READ = " (not read)";
const unescapeHtml = (text: string) =>
  text.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&amp;/g, "&");
const BLOCK_OPEN = "<details>\n<summary>";
/**
 * The context block `message` holds at `start`, as `promptWithContext` (and
 * the TUI) write them: a paste, a file snapshot, a folder listing, a page
 * element or an MCP resource the server read, each fenced. Its end, and for a paste the text pasted; undefined
 * when the text there only looks like one.
 */
function blockAt(message: string, start: number) {
  const from = start + BLOCK_OPEN.length;
  const close = message.indexOf("</summary>\n\n", from);
  if (close < 0) return undefined;
  const summary = message.slice(from, close);
  const paste = summary.startsWith("Pasted text");
  const element = summary.startsWith("Page element: ");
  const resource = summary.startsWith(RESOURCE);
  if (
    summary.includes("\n") ||
    !(
      paste ||
      element ||
      summary.startsWith("File context: ") ||
      summary.startsWith("Directory listing: ") ||
      resource
    )
  )
    return undefined;
  const body = close + "</summary>\n\n".length;
  // A page element's lines about the page come before its fenced HTML (each
  // on one line, its strings quoted); its fence is the first line that opens one.
  const marker = element ? /^(`{3,})/gm : /`{3,}(?=\n)/y;
  marker.lastIndex = body;
  const marks = marker.exec(message)?.[element ? 1 : 0];
  if (!marks) {
    // An unfenced listing, as older clients wrote one; a paste always has a fence.
    const end = paste ? -1 : message.indexOf("\n\n</details>", body);
    return end < 0 ? undefined : { end: end + "\n\n</details>".length };
  }
  // The fence is longer than any run of backticks inside, so the first
  // matching close is this block's.
  const opened = element ? marker.lastIndex : body + marks.length;
  const end = message.indexOf(`\n${marks}\n\n</details>`, opened);
  if (end < 0) return undefined;
  const inner = body + marks.length + 1;
  const content = message.slice(inner, Math.max(inner, end));
  // `<name> · <server>:<uri>`, or the address alone; the server escapes it.
  const label = unescapeHtml(summary.slice(RESOURCE.length));
  const failed = label.endsWith(NOT_READ);
  const named = failed ? label.slice(0, -NOT_READ.length) : label;
  const split = named.lastIndexOf(" · ");
  return {
    end: end + `\n${marks}\n\n</details>`.length,
    paste: paste ? content : undefined,
    resource: resource
      ? {
          name: split < 0 ? named : named.slice(0, split),
          address: split < 0 ? named : named.slice(split + 3),
          content,
          failed,
        }
      : undefined,
  };
}
/** The marker a typed line opens or closes a Markdown code fence with. */
const fenceLine = (line: string) => /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line);
/**
 * A sent message split around its pastes and the resources the server read
 * into it, the inverse of `promptWithContext` for pastes, read the way the TUI reads one (`prompt_context::parts`): a
 * context block is taken whole, so a paste-shaped block inside a file
 * snapshot stays part of that file, and nothing inside a code fence the
 * person typed counts. Text that only looks like a paste stays text.
 */
export function sentParts(message: string): SentPart[] {
  const parts: SentPart[] = [];
  let typed = 0;
  let typedFence: string | undefined;
  let lineStart = true;
  let at = 0;
  while (at < message.length) {
    const newline = message.indexOf("\n", at);
    const end = newline < 0 ? message.length : newline;
    const line = message.slice(at, end);
    if (lineStart) {
      const marker = fenceLine(line);
      if (typedFence) {
        if (marker && marker[1][0] === typedFence[0] && marker[1].length >= typedFence.length && !marker[2].trim())
          typedFence = undefined;
        at = end + 1;
        continue;
      }
      // A backtick fence's info string holds no backtick.
      if (marker && !(marker[1][0] === "`" && marker[2].includes("`"))) {
        typedFence = marker[1];
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
    } else if (block.resource) {
      if (typed < start) parts.push({ kind: "text", text: message.slice(typed, start) });
      parts.push({ kind: "resource", text: message.slice(start, block.end), ...block.resource });
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
