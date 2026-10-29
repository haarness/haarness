/*
 * The icons a server view names for its title and rows, such as a
 * marketplace resource's. The server keeps only https addresses on Haarness
 * Cloud itself, and the page's CSP loads images from nowhere else. An icon
 * that is missing or does not load leaves no box behind, and one that failed
 * is not asked for again while the app runs.
 */

const failed = new Set<string>();

/** The address to load for `icon`, when there is one worth trying. */
export function iconSource(icon: unknown): string | undefined {
  if (typeof icon !== "string" || failed.has(icon)) return undefined;
  try {
    return new URL(icon).protocol === "https:" ? icon : undefined;
  } catch {
    return undefined;
  }
}

/** Remembers an icon that did not load, so a list drawn again skips it. */
export function iconFailed(icon: string) {
  failed.add(icon);
}

/**
 * Whether a list gives every row an icon's width, so labels line up: when
 * any of its rows has an icon to show.
 */
export function rowsHaveIcons(entries: readonly { icon?: string | null }[]) {
  return entries.some((entry) => iconSource(entry.icon) !== undefined);
}
