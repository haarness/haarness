/** A quote up to this long goes into the message whole. */
const WHOLE = 1200;
/** What a longer quote keeps of each of its ends. */
const END = 480;

/** `text` cut to its first `size` characters, never inside a character
 * written as two (an emoji, a rare CJK character). */
function head(text: string, size: number) {
  const code = text.charCodeAt(size - 1);
  return text.slice(0, code >= 0xd800 && code <= 0xdbff ? size - 1 : size);
}

/** `text` cut to its last `size` characters, never inside a character. */
function tail(text: string, size: number) {
  const from = text.length - size;
  const code = text.charCodeAt(from);
  return text.slice(code >= 0xdc00 && code <= 0xdfff ? from + 1 : from);
}

/**
 * The words selected in a reply as a comment quotes them: without the space
 * around them or at the ends of lines, never two blank lines in a row, and a
 * long passage shortened to its two ends, cut between words where it has
 * them nearby, with "…" on the line between.
 */
export function replyQuote(text: string) {
  const tidy = text
    .replace(/\r\n?/g, "\n")
    .replace(/[^\S\n]+$/gm, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  if (tidy.length <= WHOLE) return tidy;
  let start = head(tidy, END);
  const space = start.search(/\s\S*$/);
  if (space > END / 2) start = start.slice(0, space);
  let end = tail(tidy, END);
  const word = end.search(/\s/);
  if (word >= 0 && word < END / 2) end = end.slice(word);
  return start.trimEnd() + "\n…\n" + end.trimStart();
}

/** The quote on one line, for the comment box's label. */
export function quoteLine(quote: string) {
  return quote.replace(/\s+/g, " ").trim();
}

/**
 * What a comment on part of a reply adds to the message: the words quoted,
 * as Markdown, then the comment. Written for the model, like the notes that
 * Review and Preview comments add.
 */
export function replyCommentNote(quote: string, comment: string) {
  const quoted = quote
    .split("\n")
    .map((line) => (line ? "> " + line : ">"))
    .join("\n");
  return "Comment on this part of your reply:\n" + quoted + "\n\n" + comment.trim();
}

/** `note` after what the message box holds, a blank line between. */
export function withNote(draft: string, note: string) {
  return draft.trim() ? draft.trimEnd() + "\n\n" + note : note;
}

/** A box on screen, in the window's pixels. */
export interface Box {
  left: number;
  right: number;
  top: number;
  bottom: number;
}

/**
 * Where the Comment button, and then its box, go in the reply's row (in the
 * row's pixels): just under the line where the selection ends (`top`), in a
 * band across the reply (`left`, `width`), starting `from` the band's left
 * edge at the selection's end, or from its right edge when the text there
 * runs right to left (`backward`). What does not fit slides back to stay
 * inside the reply.
 */
export interface CommentSpot {
  top: number;
  left: number;
  width: number;
  from: number;
  backward: boolean;
}

/**
 * `line` is the selection's last line box and `caret` where the selection
 * ends, when the page can say (a caret on that line): its side of the line
 * gives the text's direction there, so a Hebrew passage in an English
 * window ends on its left. Without one, the interface's direction (`rtl`)
 * stands in.
 */
export function commentSpot(
  line: Box,
  caret: Box | undefined,
  reply: Box,
  row: Box,
  rtl: boolean,
): CommentSpot {
  const onLine = !!caret && caret.bottom > line.top && caret.top < line.bottom;
  const backward = onLine
    ? Math.abs(caret.left - line.left) < Math.abs(caret.left - line.right)
    : rtl;
  const at = onLine ? caret.left : backward ? line.left : line.right;
  const end = Math.min(Math.max(at, reply.left), reply.right);
  return {
    top: Math.round(line.bottom - row.top),
    left: Math.round(reply.left - row.left),
    width: Math.round(reply.right - reply.left),
    from: Math.round(backward ? reply.right - end : end - reply.left),
    backward,
  };
}
