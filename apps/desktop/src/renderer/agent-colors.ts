/** Presentation-only agent colors, resolved from the active theme. */
const palette = [
  "var(--primary)",
  "var(--syntax-keyword)",
  "var(--accent)",
  "var(--green)",
  "var(--syntax-property)",
  "var(--syntax-number)",
];
const builtins: Record<string, string> = {
  build: palette[0],
  plan: palette[1],
  review: palette[2],
  explore: palette[3],
  general: palette[4],
  title: palette[5],
  compaction: palette[1],
};

export function agentColor(id: string) {
  const name = id.toLowerCase();
  if (Object.hasOwn(builtins, name)) return builtins[name];
  // Catalog ordering and project changes must not recolor a custom agent.
  let hash = 0;
  for (const character of name)
    hash = (Math.imul(hash, 31) + character.charCodeAt(0)) >>> 0;
  return palette[hash % palette.length];
}
