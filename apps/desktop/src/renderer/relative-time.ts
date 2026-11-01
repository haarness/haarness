/** Times said as a distance from now, shared by the chat and the Screenshots dialog. */

/** How long ago `at` was, in the reader's language ("5 minutes ago", "yesterday"), at any distance. */
export function relativeTime(at: number, now: number, locale: string) {
  const format = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
  // A time ahead of `now` is a clock running ahead of this one (another
  // computer's): it happened now, not "in 2 minutes".
  let value = (Math.min(at, now) - now) / 1000;
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
