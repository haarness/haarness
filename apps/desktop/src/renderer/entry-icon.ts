/*
 * The icons a server view names for its title and rows, such as a
 * marketplace resource's. The server keeps only https addresses on Haarness
 * Cloud itself, and the page's CSP loads images from nowhere else. An icon
 * that is missing or does not load leaves no box behind, and one that failed
 * is not asked for again while the app runs.
 *
 * Every icon sits on the same light tile in both themes: vendor logos are
 * drawn for white paper, and a dark mark on a transparent ground would vanish
 * on the dark theme. A logo with a transparent ground keeps a margin inside
 * the tile; an image whose corners are opaque (most avatars) fills it.
 */

const failed = new Set<string>();
/** Whether each measured icon fills its square; many rows share one logo. */
const fills = new Map<string, boolean>();

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

/** Whether `icon` was measured and fills its whole square. */
export function iconFills(icon: string) {
  return fills.get(icon) === true;
}

/** Whether `icon` still needs measuring. */
export function iconUnmeasured(icon: string) {
  return !fills.has(icon);
}

/** Records how `icon` measured; one that could not be read keeps its margin. */
export function iconMeasured(icon: string, filled: boolean | undefined) {
  fills.set(icon, filled === true);
}

/**
 * Whether an image drawn into a `size` × `size` RGBA square covers it: its
 * four corner pixels are opaque. A logo on a transparent ground has clear
 * corners even when the mark itself reaches the edges.
 */
export function cornersOpaque(rgba: ArrayLike<number>, size: number) {
  const last = size - 1;
  return [0, last, last * size, last * size + last].every((pixel) => rgba[pixel * 4 + 3] > 250);
}
