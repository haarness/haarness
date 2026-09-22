import { marked, type TokensList } from "marked";

export type MarkdownBlock = { raw: string; html: string };
export type MarkdownProjection = {
  text: string;
  blocks: MarkdownBlock[];
  tail: number;
  offset: number;
  references: boolean;
};

// Shared by all messages in the worker, including live-to-history remounts.
// Bound bytes as well as entries: one enormous answer must not pin memory.
const cache = new Map<string, string>();
let bytes = 0;
const limit = 2 * 1024 * 1024;

function render(raw: string, tokens: TokensList, references: boolean) {
  if (!references && cache.has(raw)) {
    const html = cache.get(raw)!;
    cache.delete(raw);
    cache.set(raw, html);
    return html;
  }
  const html = marked.parser(tokens, { gfm: true });
  const size = (raw.length + html.length) * 2;
  if (!references && size <= limit) {
    cache.set(raw, html);
    bytes += size;
    while (bytes > limit || cache.size > 256) {
      const key = cache.keys().next().value!;
      bytes -= (key.length + cache.get(key)!.length) * 2;
      cache.delete(key);
    }
  }
  return html;
}

export function projectMarkdown(
  previous: MarkdownProjection | undefined,
  source: string,
  live: boolean,
): MarkdownProjection {
  const text = source.replace(/\r\n?/g, "\n");
  // Definitions can resolve links anywhere in the document, even inside
  // quotes/lists. Conservatively re-lex when this syntax occurs (also in code).
  const references = text.includes("]:");
  const append = live && previous && !references && !previous.references && text.startsWith(previous.text);
  const offset = append ? previous.offset : 0;
  const prefix = append ? previous.blocks.slice(0, previous.tail) : [];
  const suffix = text.slice(offset);
  const tokens = marked.lexer(suffix, { gfm: true });
  // Do not guess offsets if a future lexer normalizes or omits source text.
  if (tokens.map((token) => token.raw).join("") !== suffix) {
    return {
      text, references: true, tail: 0, offset: 0,
      blocks: [{ raw: text, html: marked.parse(text, { async: false, gfm: true }) as string }],
    };
  }
  const blocks = [...prefix];
  let tail = prefix.length;
  let boundary = offset;
  let position = offset;
  let last = { tail, offset };
  for (const token of tokens) {
    if (token.type !== "space") {
      tail = last.tail;
      boundary = last.offset;
      last = { tail: blocks.length, offset: position };
    }
    const list = Object.assign([token], { links: tokens.links }) as TokensList;
    blocks.push({ raw: token.raw, html: render(token.raw, list, references) });
    position += token.raw.length;
  }
  // Keep two final blocks mutable. A partial next list marker initially
  // parses as a paragraph, then merges back into the preceding list.
  return { text, blocks, tail, offset: boundary, references };
}

export type MarkdownPatch = { start: number; html: string[]; count: number };
export function markdownPatch(previous: MarkdownProjection | undefined, next: MarkdownProjection): MarkdownPatch {
  let start = 0;
  while (start < next.blocks.length && previous?.blocks[start]?.html === next.blocks[start].html) start++;
  return { start, html: next.blocks.slice(start).map((block) => block.html), count: next.blocks.length };
}
