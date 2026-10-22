/** The newest line of a thought being streamed, without the Markdown marks
 * around it: what the activity line ticks through in Compact. */
export function latestThought(text: string) {
  const line =
    text
      .split("\n")
      .map((value) => value.trim())
      .findLast((value) => !!value) ?? "";
  return line
    .replace(/^[#>*\-\s]+/, "")
    .replace(/\*\*|`/g, "")
    .trim();
}

/** How long a thought took, as narrow as a step row reads it in the
 * interface language ("12s", "1m 5s", "12秒"); at least a second. */
export function thoughtDuration(ms: number, locale: string) {
  const seconds = Math.max(1, Math.round(ms / 1000));
  const unit = (value: number, name: "minute" | "second") =>
    new Intl.NumberFormat(locale, { style: "unit", unit: name, unitDisplay: "narrow" }).format(value);
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  if (!minutes) return unit(rest, "second");
  return rest ? `${unit(minutes, "minute")} ${unit(rest, "second")}` : unit(minutes, "minute");
}
