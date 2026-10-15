/**
 * Geometry for the conversation outline, a short stack of prompt dashes at
 * the top of the transcript's leading edge. Hovering it opens a panel with one
 * row per prompt, placed so the row the pointer opened on sits under it.
 * Pure functions, so the layout is tested without a window.
 */

/** Pitch of the dashes at rest: 2px dashes with an 8px gap. */
export const PITCH = 10;
/** Height of a panel row. */
export const ROW = 28;
/** Panel padding above the first row and below the last one. */
export const PAD = 6;
/** Space kept clear at the top and bottom of the lane. */
export const INSET = 12;
/** Where the stack starts: near the transcript's top, and low enough for the
 * panel's first row to open level with the first dash. */
export const TOP = 32;
/** Where the dashes sit from the leading edge. */
export const EDGE = 10;
/** At most this many dashes; longer conversations fold into even slices. */
export const FOLD_MAX = 24;
/** At most this many rows in view in the panel; it scrolls through the rest. */
export const ROWS_MAX = 16;

/** Where a prompt row starts, in scroll-content pixels. */
export type Mark = { id: string; offset: number };

/**
 * The stack at rest: its pitch and the centre of its first dash. It starts
 * at `TOP` and packs tighter only when the lane is too short to hold it.
 */
export function fold(count: number, height: number) {
  const room = Math.max(height - TOP - INSET, 0);
  const pitch = count > 1 ? Math.min(PITCH, room / (count - 1)) : 0;
  const size = pitch * Math.max(count - 1, 0);
  return { pitch, top: Math.max(INSET, Math.min(TOP, height - INSET - size)) };
}

/**
 * Whether a lane `height` tall holds the outline: the panel's smallest size
 * (one row and its padding) between the insets, opening level with a stack
 * at `top`. A shorter lane shows no stack, hit area or panel, so none of them
 * reaches past its bottom edge over the composer.
 */
export function fits(height: number, top = TOP) {
  return height - Math.max(INSET, top - PAD - ROW / 2) - INSET >= ROW + PAD * 2;
}

/**
 * The first prompt of each dash: every prompt, or past `max`, the first
 * prompt of each of `max` even slices, so a long session stays short.
 */
export function slices(count: number, max = FOLD_MAX) {
  if (count <= max) return Array.from({ length: count }, (_, i) => i);
  return Array.from({ length: max }, (_, k) => Math.floor((k * count) / max));
}

/** Index of the slice that holds prompt `i`. `starts` ascends from 0. */
export function sliceOf(starts: readonly number[], i: number) {
  let lo = 0;
  let hi = starts.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (starts[mid]! <= i) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

/**
 * The panel: its top, height, and how far its rows start scrolled. Its first
 * row is level with the first dash. It takes the list's height up to
 * `ROWS_MAX` rows and the lane's height; a longer list starts scrolled so
 * the `anchor` row sits under `y`.
 */
export function unfold(count: number, anchor: number, y: number, height: number, top = TOP) {
  const full = count * ROW + PAD * 2;
  const start = Math.max(INSET, top - PAD - ROW / 2);
  const size = Math.min(full, ROWS_MAX * ROW + PAD * 2, Math.max(height - start - INSET, ROW + PAD * 2));
  const max = full - size;
  const scroll = Math.max(0, Math.min(max, PAD + anchor * ROW + ROW / 2 - (y - start)));
  return { top: start, size, scroll, max };
}

/** The row under `y` in a panel at `top` scrolled by `scroll`, or -1. */
export function rowAt(count: number, y: number, top: number, scroll: number) {
  const i = Math.floor((y - top - PAD + scroll) / ROW);
  return i >= 0 && i < count ? i : -1;
}

/** The first and last row showing, even partly, in a panel of `size`. */
export function visible(count: number, size: number, scroll: number) {
  const first = Math.max(0, Math.floor((scroll - PAD) / ROW));
  const last = Math.min(count - 1, Math.ceil((scroll + size - PAD) / ROW) - 1);
  return [first, last] as const;
}

/** Index of the last mark at or above `y`, or -1. Offsets ascend. */
export function markAt(marks: readonly Mark[], y: number) {
  let lo = 0;
  let hi = marks.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (marks[mid]!.offset <= y) lo = mid + 1;
    else hi = mid;
  }
  return lo - 1;
}

/**
 * The prompt being read: the one whose row starts at or above the reading
 * line, or the newest one at the end of a conversation or when nothing
 * scrolls. `undefined` without marks.
 */
export function readingMark(marks: readonly Mark[], scrollTop: number, maxScroll: number, line: number) {
  if (!marks.length) return undefined;
  if (maxScroll <= 0 || scrollTop >= maxScroll - 4) return marks.at(-1)!.id;
  return marks[Math.max(markAt(marks, scrollTop + line), 0)]!.id;
}

/**
 * A prompt's title: its first written line. Context attached after the text
 * (file snapshots) is not part of it; attachment-only prompts name their
 * files, falling back to `fallback`.
 */
export function promptTitle(text: string, attachments: readonly string[], name: (value: string) => string, fallback: string) {
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    if (/^<(details|summary)\b/i.test(line)) break;
    return line;
  }
  return attachments.length ? attachments.map(name).join(", ") : fallback;
}
