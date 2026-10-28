/**
 * Text a command, a tool or a server wrote, as Desktop draws it.
 *
 * Such text may hold terminal escape sequences (colours, cursor moves, a
 * window title) and other control characters, which a page shows as
 * `[31m` or `]0;title` fragments, so output loses its sequences whole
 * (`withoutEscapes`). An approval card is read for what will run, so nothing
 * on it may be hidden or reordered: there the invisible characters show as
 * codes such as `\u{1B}` instead (`literalParts`, `literalText`). Neither
 * changes the text itself: what is stored, and what the model reads, stay as
 * written. The TUI reads such text the same way.
 */

const ESC = 0x1b;
const BEL = 0x07;

type Waiting = "introducer" | "intermediate" | "csi" | "text" | "text-escape";
type Pending = { start: number; waiting: Waiting };

/** A control character (C0, DEL or C1). */
const control = (code: number) => code < 0x20 || (code >= 0x7f && code <= 0x9f);
/** One a reader keeps: tab, line feed, carriage return. */
const kept = (code: number) => code === 0x09 || code === 0x0a || code === 0x0d;

/**
 * `text` with every escape sequence and every other control character taken
 * out: CSI (`ESC [ … m`), OSC (`ESC ] … BEL`), DCS, SOS, PM and APC
 * strings, and the two- and three-character escapes (`ESC 7`, `ESC ( B`).
 * Tab, line feed and carriage return stay. A C1 control (U+0080–U+009F) starts nothing and goes
 * on its own: a program's 8-bit control bytes reach Desktop as U+FFFD, so a
 * C1 code point here is almost always text decoded twice (`â\u{80}\u{9c}`
 * for an opening quote), and the line around it shows.
 *
 * Removing a sequence can complete another around it (`ESC ESC[22m[32m`), and
 * that one goes too, as removing them over and over until none is left
 * would: an unfinished sequence waits on a stack while the ones inside it
 * complete. A string that never ends stops where its line does, and the
 * lines below it still show. Linear in the length of `text`.
 */
export function withoutEscapes(text: string): string {
  let found = false;
  for (let index = 0; index < text.length; index++) {
    const code = text.charCodeAt(index);
    if (control(code) && !kept(code)) {
      found = true;
      break;
    }
  }
  if (!found) return text;
  // Code points kept so far; a pending sequence starts at an index in it.
  const out: number[] = [];
  const pending: Pending[] = [];
  const begin = (waiting: Waiting, introducer: number) => {
    pending.push({ start: out.length, waiting });
    // Held until the sequence completes, so an unfinished one can still be
    // shown.
    out.push(introducer);
  };
  const finish = () => {
    const done = pending.pop();
    if (done) out.length = done.start;
  };
  for (const character of text) {
    const code = character.codePointAt(0)!;
    for (;;) {
      const open = pending.at(-1);
      if (!open) {
        if (code === ESC) begin("introducer", code);
        else out.push(code);
        break;
      }
      if (code === ESC && (open.waiting === "introducer" || open.waiting === "intermediate" || open.waiting === "csi")) {
        // A sequence inside an unfinished one: once it is removed, the outer
        // one reads on from what follows it.
        begin("introducer", code);
        break;
      }
      if (open.waiting === "introducer") {
        if (code === 0x5b) open.waiting = "csi";
        else if (code === 0x5d || code === 0x50 || code === 0x58 || code === 0x5e || code === 0x5f) open.waiting = "text";
        else if (code >= 0x20 && code <= 0x2f) open.waiting = "intermediate";
        else if (code >= 0x30 && code <= 0x7e) {
          finish();
          break;
        } else {
          // Not an escape after all: none of the unfinished ones under it can
          // end now, since this one never will.
          pending.length = 0;
          continue;
        }
      } else if (open.waiting === "intermediate" || open.waiting === "csi") {
        const more = open.waiting === "csi" ? 0x3f : 0x2f;
        const first = open.waiting === "csi" ? 0x40 : 0x30;
        if (code >= first && code <= 0x7e) {
          finish();
          break;
        }
        if (!(code >= 0x20 && code <= more)) {
          pending.length = 0;
          continue;
        }
      } else if (open.waiting === "text-escape") {
        if (code === 0x5c) {
          finish();
          break;
        }
        // An escape that is not ST ends the string, as a terminal reads it,
        // and begins a sequence of its own.
        pending.pop();
        out.length = open.start;
        begin("introducer", ESC);
        continue;
      } else {
        if (code === BEL) {
          finish();
          break;
        }
        if (code === ESC) open.waiting = "text-escape";
        else if (code === 0x0a) {
          // Never ended on its line: drop it and keep the line break for
          // whatever waits under it.
          pending.pop();
          out.length = open.start;
          continue;
        }
      }
      out.push(code);
      break;
    }
  }
  // A string still open at the end ends with the text: it is dropped. An
  // unfinished escape is only its first characters, shown as written below
  // without the control character that began it.
  const last = pending.at(-1);
  if (last && (last.waiting === "text" || last.waiting === "text-escape")) out.length = last.start;
  let shown = "";
  for (const code of out) if (!control(code) || kept(code)) shown += String.fromCodePoint(code);
  return shown;
}

/**
 * Whether an approval card shows the character at `code` as a code. That is
 * any control character other than tab and line feed (a carriage return
 * always, also at the end of a line, where a shell keeps it in the word
 * before it: `cd dist` then CR LF goes into a folder named `dist` and a
 * carriage return), an invisible format character or another
 * default-ignorable one, such as the direction marks, overrides and isolates
 * that reorder what is shown and the zero-width spaces that hide a
 * difference, and the line and paragraph separators. Left as they are: the
 * joiners U+200C and U+200D and the variation selectors U+FE00–U+FE0F and
 * U+E0100–U+E01EF, without which emoji sequences and many writing
 * systems would not join or pick their glyphs, and the format characters
 * that draw a visible sign (U+0600–U+0605, U+06DD).
 */
export function showsAsCode(code: number): boolean {
  if (code < 0xad) return control(code) && code !== 0x09 && code !== 0x0a;
  if (control(code)) return true;
  return INVISIBLE.some(([from, to]) => code >= from && code <= to);
}

/** Invisible format and other default-ignorable code points, as inclusive
 * ranges. */
const INVISIBLE: ReadonlyArray<readonly [number, number]> = [
  [0xad, 0xad],
  [0x34f, 0x34f],
  [0x61c, 0x61c],
  [0x115f, 0x1160],
  [0x17b4, 0x17b5],
  [0x180b, 0x180f],
  [0x200b, 0x200b],
  [0x200e, 0x200f],
  [0x2028, 0x202e],
  [0x2060, 0x206f],
  [0x3164, 0x3164],
  [0xfeff, 0xfeff],
  [0xffa0, 0xffa0],
  [0xfff0, 0xfffb],
  [0x13430, 0x1343f],
  [0x1bca0, 0x1bca3],
  [0x1d173, 0x1d17a],
  [0xe0000, 0xe00ff],
  [0xe01f0, 0xe0fff],
];

/** The flags of England, Scotland and Wales are a black flag followed by
 * tag characters that spell the region and a cancel tag. Those tags draw the
 * flag, so they stay; every other tag character is a code. */
const BLACK_FLAG = 0x1f3f4;
const FLAG_TAGS = [
  "\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}",
  "\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}",
  "\u{e0067}\u{e0062}\u{e0077}\u{e006c}\u{e0073}\u{e007f}",
];

/** A character as a code a reader can see: `\u{1B}`. */
const codeOf = (code: number) => `\\u{${code.toString(16).toUpperCase()}}`;

/** `text` cut into what shows as written and the codes `showsAsCode` names,
 * for a card that draws the codes quieter than the text around them. */
export function literalParts(text: string): { text: string; code: boolean }[] {
  let any = false;
  for (const character of text)
    if (showsAsCode(character.codePointAt(0)!)) {
      any = true;
      break;
    }
  if (!any) return [{ text, code: false }];
  const parts: { text: string; code: boolean }[] = [];
  let run = "";
  for (let index = 0; index < text.length; ) {
    const code = text.codePointAt(index)!;
    const character = String.fromCodePoint(code);
    index += character.length;
    const tags = code === BLACK_FLAG ? FLAG_TAGS.find((tags) => text.startsWith(tags, index)) : undefined;
    if (tags) {
      run += character + tags;
      index += tags.length;
    } else if (!showsAsCode(code)) run += character;
    else {
      if (run) parts.push({ text: run, code: false });
      run = "";
      parts.push({ text: codeOf(code), code: true });
    }
  }
  if (run) parts.push({ text: run, code: false });
  return parts;
}

/** `text` with each character `showsAsCode` names written as its code, for
 * places that take plain text (a row's target, a menu's detail line). */
export function literalText(text: string): string {
  return literalParts(text)
    .map((part) => part.text)
    .join("");
}
