import { marked, type Token, type TokensList } from "marked";

export type MarkdownBlock = { raw: string; html: string };
export type MarkdownProjection = {
  text: string;
  blocks: MarkdownBlock[];
  tail: number;
  offset: number;
  references: boolean;
  breaks: boolean;
};

/**
 * HTML elements that show their content where they stand, which a reply may
 * write bare (`<kbd>`, `<br>`, `<ul>` in a table cell) and keep as markup.
 */
const SHOWN = new Set([
  "a", "abbr", "address", "article", "aside", "b", "bdi", "bdo", "big", "blockquote", "br", "caption",
  "center", "cite", "code", "col", "colgroup", "data", "dd", "del", "details", "dfn", "div", "dl", "dt",
  "em", "figcaption", "figure", "footer", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hgroup", "hr",
  "i", "img", "ins", "kbd", "li", "main", "mark", "nav", "ol", "p", "pre", "q", "rp", "rt", "ruby", "s",
  "samp", "section", "small", "span", "strike", "strong", "sub", "summary", "sup", "table", "tbody", "td",
  "tfoot", "th", "thead", "time", "tr", "tt", "u", "ul", "var", "wbr",
]);
const BARE_TAG = /^<(\/?)([A-Za-z][\w:-]*)\s*(\/?)>$/;

/**
 * Inside a paragraph, a bare tag that is not such an element is a word of the
 * reply, as the TUI shows it: `<path>`, `<file>`, `<svg>`, `<title>`. Read as
 * markup, the sanitizer drops it, and with `<path>` or `<text>` everything
 * after it in the block (an SVG name outside an svg is removed whole); `<svg>`
 * and `<title>` hide what follows. Its closing tag in the same run of text
 * goes with it. A tag with attributes is markup and stays, so an inline SVG
 * drawing still draws.
 */
function literalTags(tokens: Token[]) {
  const open: string[] = [];
  tokens.forEach((token, index) => {
    if (token.type !== "html" || token.block) return;
    const tag = BARE_TAG.exec(token.text);
    const name = tag?.[2].toLowerCase();
    if (!tag || !name || SHOWN.has(name)) return;
    if (tag[1]) {
      const at = open.lastIndexOf(name);
      if (at < 0) return;
      open.splice(at, 1);
    } else if (!tag[3]) open.push(name);
    tokens[index] = { type: "text", raw: token.raw, text: token.text, escaped: false };
  });
}

/** A label's tokens with every link in them, at any depth, replaced by its own tokens. */
const linkless = (tokens: Token[]): Token[] =>
  tokens.flatMap((token): Token[] =>
    token.type === "link"
      ? linkless(token.tokens ?? [])
      : "tokens" in token && token.tokens
        ? [{ ...token, tokens: linkless(token.tokens) } as Token]
        : [token],
  );

/**
 * Runs on every token before it is drawn (see `prepare`). A link inside a
 * link's label (`[<dev@example.com>](url)`, `[[a](x)](y)`) keeps only its
 * text: an anchor inside an anchor is split by the HTML parser into an empty
 * link to the label's address and a visible one to the inner address.
 */
function replyToken(token: Token) {
  if (token.type === "link" && token.tokens) token.tokens = linkless(token.tokens);
  if (token.type === "table") {
    for (const cell of [...token.header, ...token.rows.flat()]) literalTags(cell.tokens);
    return;
  }
  if ("tokens" in token && token.tokens) literalTags(token.tokens);
}

/**
 * Replies keep the model's single line breaks, as the TUI does: agent output
 * often relies on its line structure. A Markdown file keeps standard
 * paragraphs, where a lone newline is a space.
 */
export const markdownOptions = (breaks: boolean) => ({ gfm: true, breaks, walkTokens: replyToken });

/** What `marked.parse` does with `walkTokens` between lexing and drawing. */
function prepare<T extends Token[]>(tokens: T) {
  marked.walkTokens(tokens, replyToken);
  return tokens;
}

// Shared by all messages in the worker, including live-to-history remounts.
// Bound bytes as well as entries: one enormous answer must not pin memory.
const cache = new Map<string, string>();
let bytes = 0;
const limit = 2 * 1024 * 1024;

function render(raw: string, tokens: TokensList, references: boolean, breaks: boolean) {
  const key = (breaks ? "b:" : "p:") + raw;
  if (!references && cache.has(key)) {
    const html = cache.get(key)!;
    cache.delete(key);
    cache.set(key, html);
    return html;
  }
  const html = marked.parser(prepare(tokens), markdownOptions(breaks));
  const size = (key.length + html.length) * 2;
  if (!references && size <= limit) {
    cache.set(key, html);
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
  breaks = true,
): MarkdownProjection {
  const text = source.replace(/\r\n?/g, "\n");
  // Definitions can resolve links anywhere in the document, even inside
  // quotes/lists. Conservatively re-lex when this syntax occurs (also in code).
  const references = text.includes("]:");
  const append = live && previous && !references && !previous.references && previous.breaks === breaks && text.startsWith(previous.text);
  const offset = append ? previous.offset : 0;
  const prefix = append ? previous.blocks.slice(0, previous.tail) : [];
  const suffix = text.slice(offset);
  const tokens = marked.lexer(suffix, markdownOptions(breaks));
  // Do not guess offsets if a future lexer normalizes or omits source text.
  if (tokens.map((token) => token.raw).join("") !== suffix) {
    return {
      text, references: true, tail: 0, offset: 0, breaks,
      blocks: [{ raw: text, html: marked.parse(text, { async: false, ...markdownOptions(breaks) }) as string }],
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
    blocks.push({ raw: token.raw, html: render(token.raw, list, references, breaks) });
    position += token.raw.length;
  }
  // Keep two final blocks mutable. A partial next list marker initially
  // parses as a paragraph, then merges back into the preceding list.
  return { text, blocks, tail, offset: boundary, references, breaks };
}

export type MarkdownPatch = { start: number; html: string[]; count: number };
export function markdownPatch(previous: MarkdownProjection | undefined, next: MarkdownProjection): MarkdownPatch {
  let start = 0;
  while (start < next.blocks.length && previous?.blocks[start]?.html === next.blocks[start].html) start++;
  return { start, html: next.blocks.slice(start).map((block) => block.html), count: next.blocks.length };
}
