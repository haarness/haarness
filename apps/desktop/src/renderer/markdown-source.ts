import { marked, type Token, type Tokens } from "marked";
import { markdownOptions } from "./markdown-project";

/**
 * Which source lines of a Markdown file a selection in its rendered Preview
 * came from. The Preview draws one block per top-level token of the same
 * lexer (markdown-project.ts), so a block's place gives its token, and the
 * token's raw text gives its lines. Lists, quotes and tables narrow further:
 * a list item, a quoted paragraph or a table row has lines of its own, and a
 * line inside a fenced code block is that line. Line numbers start at 1 and
 * ranges include both ends.
 */

export type SourceRange = { from: number; to: number };
/** How a rendered element sits in its container: a list item, a quote's child, a table row. */
export type SegmentKind = "item" | "child" | "row";
export type SourceNode = SourceRange & {
  type: string;
  kind?: SegmentKind;
  parts?: SourceNode[];
  /** The first line of a code block's code (after an opening fence). */
  code?: number;
  /** The last line of a code block's code (before a closing fence). */
  codeEnd?: number;
};
export type Segment = { kind: SegmentKind; index: number };
/** One end of a selection: its block, the containers it sits in, and its line in a code block. */
export type Boundary = { block: number; path: Segment[]; codeLine?: number };

const newlines = (text: string) => {
  let count = 0;
  for (let index = text.indexOf("\n"); index >= 0; index = text.indexOf("\n", index + 1)) count++;
  return count;
};

/** A token's lines, from its first line: its raw text without trailing blank lines. */
function node(token: Token, from: number, kind?: SegmentKind): SourceNode {
  const content = token.raw.replace(/\s+$/, "");
  const result: SourceNode = { type: token.type, from, to: from + newlines(content), ...(kind && { kind }) };
  if (token.type === "list") {
    // An item's raw text is the source's, so items follow one another line by line.
    let line = from;
    result.parts = (token as Tokens.List).items.map((item) => {
      const part = node(item as unknown as Token, line, "item");
      line += newlines(item.raw);
      return part;
    });
  } else if (token.type === "list_item") {
    // An item's own tokens are lexed from its text without the marker and
    // indentation, which keeps every line where it was.
    let line = from;
    const nested: SourceNode[] = [];
    for (const child of (token as Tokens.ListItem).tokens) {
      if (child.type === "list") nested.push(...(node(child, line).parts ?? []));
      line += newlines(child.raw);
    }
    if (nested.length) result.parts = nested;
  } else if (token.type === "blockquote") {
    // Quoted tokens are lexed without their `>` marks, one line for each line.
    let line = from;
    const parts: SourceNode[] = [];
    for (const child of (token as Tokens.Blockquote).tokens) {
      if (child.type !== "space") parts.push(node(child, line, "child"));
      line += newlines(child.raw);
    }
    result.parts = parts;
  } else if (token.type === "table") {
    // A table's header row comes with its delimiter row; each body row is one line.
    const rows = (token as Tokens.Table).rows.length;
    result.parts = [
      { type: "row", kind: "row", from, to: Math.min(from + 1, result.to) },
      ...Array.from({ length: rows }, (_, index) => ({
        type: "row",
        kind: "row" as const,
        from: Math.min(from + 2 + index, result.to),
        to: Math.min(from + 2 + index, result.to),
      })),
    ];
  } else if (token.type === "code") {
    const fenced = /^ {0,3}(`{3,}|~{3,})/.test(token.raw);
    const closed = fenced && /\n {0,3}(`{3,}|~{3,})[ \t]*$/.test(content);
    result.code = fenced ? Math.min(from + 1, result.to) : from;
    result.codeEnd = closed ? Math.max(result.code, result.to - 1) : result.to;
  }
  return result;
}

/**
 * The Preview's blocks with their source lines, one for each block the
 * Preview draws (blank stretches included). When the lexer's tokens do not
 * cover the text exactly, the Preview draws it as one block, and so does this.
 */
export function sourceBlocks(source: string): SourceNode[] {
  const text = source.replace(/\r\n?/g, "\n");
  const tokens = marked.lexer(text, markdownOptions(false));
  if (tokens.map((token) => token.raw).join("") !== text)
    return [{ type: "document", from: 1, to: Math.max(1, newlines(text.replace(/\s+$/, "")) + 1) }];
  let line = 1;
  return tokens.map((token) => {
    const block = node(token, line);
    line += newlines(token.raw);
    return block;
  });
}

/** The deepest part of its block that a boundary's containers lead to. */
function resolve(blocks: SourceNode[], boundary: Boundary): SourceNode | undefined {
  let current = blocks[boundary.block];
  if (!current) return undefined;
  for (const segment of boundary.path) {
    const parts = current.parts;
    const next = parts?.[0]?.kind === segment.kind ? parts[segment.index] : undefined;
    if (!next) break;
    current = next;
  }
  return current;
}

/** The line a boundary in a code block sits on, or none outside code. */
function codeLine(node: SourceNode, boundary: Boundary) {
  if (node.code === undefined || boundary.codeLine === undefined) return undefined;
  return Math.max(node.code, Math.min(node.codeEnd ?? node.to, node.code + boundary.codeLine));
}

/** A source line as it reads rendered: without block marks, inline marks and link targets. */
export function plainLine(line: string) {
  return line
    .replace(/^\s{0,3}(?:>\s?)*/, "")
    .replace(/^\s*(?:#{1,6}\s+|[-*+]\s+(?:\[[ xX]\]\s+)?|\d+[.)]\s+)/, "")
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[*_~`|\\]/g, "")
    .replace(/\s+/g, " ")
    .trim();
}
const plain = (text: string) => text.replace(/\s+/g, " ").trim();

/**
 * Narrows a range inside one block to the lines the selected text is on: a
 * phrase on one line of a paragraph is that line. Text that cannot be found,
 * or that several lines hold, keeps the whole range.
 */
function narrow(lines: string[], range: SourceRange, selected: string): SourceRange {
  const wanted = plain(selected);
  if (!wanted || range.from === range.to) return range;
  const candidates = lines.slice(range.from - 1, range.to).map(plainLine);
  // On exactly one line: that line. On several, the text cannot tell which.
  const matches = candidates.flatMap((line, index) => (line.includes(wanted) ? [index] : []));
  if (matches.length === 1) return { from: range.from + matches[0], to: range.from + matches[0] };
  if (matches.length > 1) return range;
  // Across lines: from the line whose end begins the selection to the line
  // whose beginning ends it.
  const overlaps = (line: string, fits: (piece: string) => boolean, piece: (length: number) => string) => {
    for (let length = Math.min(line.length, wanted.length); length >= Math.min(6, line.length); length--)
      if (length > 0 && fits(piece(length))) return true;
    return false;
  };
  const first = candidates.findIndex((line) =>
    overlaps(line, (piece) => wanted.startsWith(piece), (length) => line.slice(-length)),
  );
  let last = -1;
  for (let index = candidates.length - 1; index > first && first >= 0; index--) {
    const line = candidates[index];
    if (overlaps(line, (piece) => wanted.endsWith(piece), (length) => line.slice(0, length))) {
      last = index;
      break;
    }
  }
  if (first >= 0 && last > first) return { from: range.from + first, to: range.from + last };
  return range;
}

/**
 * The source lines of a selection in the Preview, from where its two ends sit
 * in the rendered blocks and the text it covers.
 */
export function selectionLines(
  source: string,
  blocks: SourceNode[],
  start: Boundary,
  end: Boundary,
  selected: string,
): SourceRange | undefined {
  const [first, last] =
    start.block < end.block || (start.block === end.block && compare(start, end) <= 0) ? [start, end] : [end, start];
  const from = resolve(blocks, first);
  const to = resolve(blocks, last);
  if (!from || !to) return undefined;
  const fromCode = codeLine(from, first);
  const toCode = codeLine(to, last);
  const range = { from: fromCode ?? from.from, to: toCode ?? to.to };
  if (range.to < range.from) return { from: range.to, to: range.from };
  // Both ends in one paragraph, item, heading or row: the text narrows it.
  if (from === to && fromCode === undefined && toCode === undefined)
    return narrow(source.replace(/\r\n?/g, "\n").split("\n"), range, selected);
  return range;
}

function compare(a: Boundary, b: Boundary) {
  const length = Math.min(a.path.length, b.path.length);
  for (let index = 0; index < length; index++)
    if (a.path[index].index !== b.path[index].index) return a.path[index].index - b.path[index].index;
  return (a.codeLine ?? 0) - (b.codeLine ?? 0);
}
