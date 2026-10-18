/** Picking screenshots in the Screenshots dialog: what each click does and how a tile dates itself. */

/** Images one message can take, as the composer's own attachment limit. */
export const PICK_LIMIT = 20;

/**
 * A click on a screenshot: picked ones are numbered in the order they were
 * clicked and go into the message in that order; clicking one again takes it
 * out and closes the gap. Past the limit, a click changes nothing.
 */
export function toggleSelection(selected: number[], index: number, limit = PICK_LIMIT): number[] {
  if (selected.includes(index)) return selected.filter((value) => value !== index);
  return selected.length >= limit ? selected : [...selected, index];
}

/** How long ago a screenshot was taken, in the reader's language ("5 minutes ago"). */
export function relativeTime(modified: number, now: number, locale: string) {
  const format = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
  let value = (modified - now) / 1000;
  const steps: [Intl.RelativeTimeFormatUnit, number][] = [
    ["second", 60],
    ["minute", 60],
    ["hour", 24],
    ["day", 7],
    ["week", 4.35],
    ["month", 12],
  ];
  for (const [unit, size] of steps) {
    if (Math.abs(value) < size)
      return unit === "second" ? format.format(0, "second") : format.format(Math.round(value), unit);
    value /= size;
  }
  return format.format(Math.round(value), "year");
}
