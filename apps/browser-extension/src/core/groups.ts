/**
 * Every chat gets its own tab group, named after the chat, in a colour that
 * stays the same for that chat. Subagents get groups of their own, named after
 * the subagent's chat. Group titles may sync to the user's Google account when
 * tab group sync is on, which is why they hold only the chat's title.
 */

export const GROUP_COLORS = ["blue", "green", "purple", "cyan", "orange", "pink", "yellow"] as const;
export type GroupColor = (typeof GROUP_COLORS)[number];

const MAX_TITLE = 40;

export function groupTitle(title: string | undefined): string {
  const clean = (title ?? "").replace(/\s+/g, " ").trim();
  if (!clean) return "Haarness";
  if (clean.length <= MAX_TITLE) return clean;
  const cut = clean.slice(0, MAX_TITLE - 1);
  const space = cut.lastIndexOf(" ");
  return (space > 24 ? cut.slice(0, space) : cut).replace(/[\s,.;:–-]+$/, "") + "…";
}

export function groupColor(thread: string): GroupColor {
  let hash = 2_166_136_261;
  for (let index = 0; index < thread.length; index++) {
    hash ^= thread.charCodeAt(index);
    hash = Math.imul(hash, 16_777_619);
  }
  return GROUP_COLORS[(hash >>> 0) % GROUP_COLORS.length];
}
