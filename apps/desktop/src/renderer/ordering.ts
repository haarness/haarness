// Presentation order is local desktop state, keyed by collection and workspace.
// Domain timestamps and ownership never change when a user moves a row.
export type Ordering = Record<string, string[]>;

export function readOrdering(value: unknown): Ordering {
  if (!value || typeof value !== "object") return {};
  const raw = value as { version?: unknown; lists?: unknown };
  if (raw.version !== 1 || !raw.lists || typeof raw.lists !== "object")
    return {};
  return Object.fromEntries(
    Object.entries(raw.lists).flatMap(([key, ids]) =>
      key.startsWith("list:") &&
      Array.isArray(ids) &&
      ids.every((id) => typeof id === "string")
        ? [[key, [...new Set(ids)]]]
        : [],
    ),
  );
}

export function orderedIds(
  ids: string[],
  saved: string[] = [],
  newcomers: "start" | "end" = "end",
) {
  const available = new Set(ids);
  const known = new Set(saved);
  const existing = [...new Set(saved)].filter((id) => available.has(id));
  const added = [...available].filter((id) => !known.has(id));
  return newcomers === "start"
    ? [...added, ...existing]
    : [...existing, ...added];
}

// Only visible slots move: filtered/archived/temporarily closed entries retain
// their place and return there when shown again.
export function moveInOrder(
  saved: string[],
  visible: string[],
  id: string,
  target: number,
): string[] {
  const from = visible.indexOf(id);
  if (from < 0 || !Number.isInteger(target)) return saved;
  const next = [...visible];
  next.splice(from, 1);
  next.splice(Math.max(0, Math.min(target, next.length)), 0, id);
  const visibleSet = new Set(visible);
  const slots = [...new Set([...saved, ...visible])];
  let index = 0;
  return slots.map((entry) => (visibleSet.has(entry) ? next[index++] : entry));
}
