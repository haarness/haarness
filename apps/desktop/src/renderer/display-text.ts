/**
 * Text a command, a tool or a server wrote, as Desktop shows it.
 *
 * Such text can carry terminal escape sequences (colours, cursor moves, a
 * window title) and other control characters, which a page shows as
 * `[31m` or `]0;title` fragments, so output loses its sequences whole
 * (`withoutEscapes`). The text itself is never changed: what is stored, and
 * what the model reads, stay as written. The TUI reads such text the same
 * way.
 */

const ESC = 0x1b;
const BEL = 0x07;
const C1_CSI = 0x9b;
const C1_ST = 0x9c;
/** The C1 introducers of OSC, DCS, SOS, PM and APC strings. */
const C1_STRINGS = new Set([0x9d, 0x90, 0x98, 0x9e, 0x9f]);

type Waiting = "introducer" | "intermediate" | "csi" | "text" | "text-escape";
type Pending = { start: number; waiting: Waiting };

/** A control character (C0, DEL or C1). */
const control = (code: number) => code < 0x20 || (code >= 0x7f && code <= 0x9f);
/** One a reader keeps: tab, line feed, carriage return. */
const kept = (code: number) => code === 0x09 || code === 0x0a || code === 0x0d;

/**
 * `text` without its escape sequences and stray control characters: CSI
 * (`ESC [ … m`), OSC (`ESC ] … BEL`), DCS, SOS, PM and APC strings, the two-
 * and three-character escapes (`ESC 7`, `ESC ( B`) and the C1 forms of all
 * of them. Tab, line feed and carriage return stay.
 *
 * Removing a sequence can complete another around it (`ESC ESC[22m[32m`), and
 * that one goes too, as removing them over and over until none is left
 * would: an unfinished sequence waits on a stack while the ones inside it
 * complete. A string that never ends stops at the end of its line, so the
 * lines after it still show. Linear in the length of `text`.
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
        else if (code === C1_CSI) begin("csi", code);
        else if (C1_STRINGS.has(code)) begin("text", code);
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
        if (code === BEL || code === C1_ST) {
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
