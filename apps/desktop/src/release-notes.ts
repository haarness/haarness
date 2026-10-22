/** The most release notes the update confirm shows; a feed could carry a whole changelog. */
const MAX_NOTES = 20_000;

/**
 * An update feed's release notes as one Markdown text, or "" when the feed
 * has none: electron-updater gives a string (Markdown, or HTML from some
 * feeds; the renderer sanitizes either) or, with a full changelog, one note
 * per version, newest first.
 */
export function releaseNotesText(value: unknown): string {
  let text = "";
  if (typeof value === "string") text = value.trim();
  else if (Array.isArray(value))
    text = value
      .filter(
        (entry): entry is { version: string; note: string } =>
          !!entry &&
          typeof entry === "object" &&
          typeof entry.version === "string" &&
          typeof entry.note === "string" &&
          !!entry.note.trim(),
      )
      .map((entry) => `### ${entry.version}\n\n${entry.note.trim()}`)
      .join("\n\n");
  return text.length > MAX_NOTES ? text.slice(0, MAX_NOTES - 1).trimEnd() + "…" : text;
}
