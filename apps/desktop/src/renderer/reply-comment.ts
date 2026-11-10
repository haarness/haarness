/** Quotes no longer than this many characters are copied in full. */
const WHOLE = 1200;
/** Past that, this many characters survive from the start and from the end. */
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

/** The longest a quote reads in the comment box's heading, and in its name
 * for a screen reader. */
const LABEL = 80;

/** The quote on one line, for the comment box's heading: at most 80
 * characters, ending in "…" when there was more. */
export function quoteLine(quote: string) {
  const line = quote.replace(/\s+/g, " ").trim();
  if (line.length <= LABEL) return line;
  return head(line, LABEL - 1).trimEnd() + "…";
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
 * Where the Comment button and its box go, in the pixels of the reply's row.
 * Both sit in a strip as wide as the reply (`left`, `width`), either just
 * under the selection's last line (`under`) or, for a box that has no room
 * there, just over its first (`over`). Along the strip they start `from` its
 * left edge, or its right edge when `backward`: at the selection's end, which
 * is the line's right end in left-to-right text and its left end in
 * right-to-left text. `footer` is where the button goes instead when under
 * the last line it would sit on the reply's own row of actions (its time,
 * Copy, Fork): in that row, centred on it, after the actions.
 */
export interface CommentSpot {
  left: number;
  width: number;
  under: number;
  over: number;
  from: number;
  backward: boolean;
  footer?: { top: number; from: number; backward: boolean };
}

/** What `commentSpot` measures from. */
export interface SpotInput {
  /** The selection's first and last line boxes. */
  first: Box;
  last: Box;
  /** The caret where the selection ends, when the page can place one. */
  caret?: Box;
  /** The reply's text, and the transcript row holding it. */
  reply: Box;
  row: Box;
  /** The reply's row of actions under its text, and those actions. */
  footer?: Box;
  actions?: Box;
  /** The interface reads right to left. */
  rtl: boolean;
  /** The gap left under a line, the button's height and the space kept
   * after the actions, in pixels. */
  gap: number;
  button: number;
  space: number;
}

/**
 * The caret's side of the last line gives the text's direction where the
 * selection ends, so a Hebrew passage in an English window ends on its left.
 * Without a caret on that line, the interface's direction stands in.
 */
export function commentSpot(input: SpotInput): CommentSpot {
  const { first, last, caret, reply, row, rtl } = input;
  const onLine = !!caret && caret.bottom > last.top && caret.top < last.bottom;
  const backward = onLine
    ? Math.abs(caret.left - last.left) < Math.abs(caret.left - last.right)
    : rtl;
  const at = onLine ? caret.left : backward ? last.left : last.right;
  const end = Math.min(Math.max(at, reply.left), reply.right);
  const spot: CommentSpot = {
    left: Math.round(reply.left - row.left),
    width: Math.round(reply.right - reply.left),
    under: Math.round(last.bottom - row.top),
    over: Math.round(first.top - row.top),
    from: Math.round(backward ? reply.right - end : end - reply.left),
    backward,
  };
  const top = last.bottom + input.gap;
  const footer = input.footer;
  if (footer && top < footer.bottom && top + input.button > footer.top) {
    // Measured from the interface's leading edge, as the row's actions are.
    const lead = rtl ? reply.right - end : end - reply.left;
    const actions = input.actions;
    const after = actions
      ? (rtl ? reply.right - actions.left : actions.right - reply.left) + input.space
      : 0;
    spot.footer = {
      top: Math.round(footer.top - row.top + (footer.bottom - footer.top - input.button) / 2),
      from: Math.round(Math.max(lead, after)),
      backward: rtl,
    };
  }
  return spot;
}
