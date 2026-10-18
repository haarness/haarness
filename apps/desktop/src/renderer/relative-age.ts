const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/**
 * How long ago `then` was, as narrow as a sidebar row can hold: `now` (the
 * caller's translated word) under a minute, then minutes, hours, days and
 * weeks in the reader's language ("5m", "5 Min.", "5分钟"), and past four
 * weeks the date itself ("Sep 3", or "Sep 2025" in an earlier year), which
 * reads better than a count of months.
 */
export function relativeAge(then: string, now: number, locale: string, nowWord: string) {
  const at = Date.parse(then);
  if (!Number.isFinite(at)) return "";
  const age = Math.max(0, now - at);
  const unit = (value: number, name: "minute" | "hour" | "day" | "week") =>
    new Intl.NumberFormat(locale, { style: "unit", unit: name, unitDisplay: "narrow" }).format(
      Math.floor(value),
    );
  if (age < MINUTE) return nowWord;
  if (age < HOUR) return unit(age / MINUTE, "minute");
  if (age < DAY) return unit(age / HOUR, "hour");
  if (age < 7 * DAY) return unit(age / DAY, "day");
  if (age < 28 * DAY) return unit(age / (7 * DAY), "week");
  const date = new Date(at);
  return date.toLocaleDateString(
    locale,
    date.getFullYear() === new Date(now).getFullYear()
      ? { month: "short", day: "numeric" }
      : { month: "short", year: "numeric" },
  );
}
