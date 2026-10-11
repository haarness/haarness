/**
 * Chrome approvals the user can answer from the toolbar popup. The bridge
 * forwards only explicit Chrome gates, whose patterns read
 * `chrome-<category>:<thread>:<origin>` (optionally behind `browser_site:`).
 */
import { key } from "./i18n";

export type ApprovalCategory = "browsing" | "downloads" | "uploads" | "scripts";

export interface ApprovalLine {
  category: ApprovalCategory;
  origin: string;
}

const PATTERN = /(?:^|:)chrome-(browsing|downloads|uploads|scripts):([0-9a-f-]{36}):(.+)$/i;

export function parsePattern(pattern: string): ApprovalLine | undefined {
  const match = PATTERN.exec(pattern);
  if (!match) return;
  return { category: match[1].toLowerCase() as ApprovalCategory, origin: match[3] };
}

/** One line per distinct request, in a stable order: browsing first. */
export function approvalLines(patterns: string[]): ApprovalLine[] {
  const order: ApprovalCategory[] = ["browsing", "scripts", "downloads", "uploads"];
  const seen = new Set<string>();
  return patterns
    .map(parsePattern)
    .filter((line): line is ApprovalLine => {
      if (!line) return false;
      const id = line.category + " " + line.origin;
      if (seen.has(id)) return false;
      seen.add(id);
      return true;
    })
    .sort((a, b) => order.indexOf(a.category) - order.indexOf(b.category));
}

/** English keys for each line, translated where they render. */
export const approvalLineKeys: Record<ApprovalCategory, string> = {
  browsing: key("Use {origin} in Chrome"),
  downloads: key("Send downloads from {origin} in Chrome to the agent"),
  uploads: key("Upload files to {origin} in Chrome"),
  scripts: key("Run scripts on {origin} in Chrome"),
};

/** The toolbar badge: "!" while Chrome waits for the user, else how many chats are working. */
export function badgeText(working: number, waiting: number): string {
  if (waiting > 0) return "!";
  return working > 0 ? String(Math.min(working, 99)) : "";
}
