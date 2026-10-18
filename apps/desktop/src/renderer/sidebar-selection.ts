import { createSignal } from "solid-js";

/**
 * Select chats: a mode of one sidebar list (a project, or the flat list) in
 * which clicking a chat row picks it instead of opening it, so several can be
 * archived at once, and worktree headings can be picked for removal. Haarness
 * has no permanent chat deletion; archiving is the reversible way out.
 */
export interface Selection {
  /** The list the mode belongs to: a project's id, or "all" for the flat list. */
  scope: string;
  /** Picked chat rows, by sidebar key. */
  chats: string[];
  /** Picked worktrees, by sidebar location id. */
  trees: string[];
  /** The row the last plain click picked or dropped; Shift extends from it. */
  anchor?: string;
}

export const emptySelection = (scope: string): Selection => ({
  scope,
  chats: [],
  trees: [],
});

/**
 * A click that starts picking from an idle list, the way a macOS list adds
 * to its selection: Command-click on macOS, Control-click elsewhere (on
 * macOS Control-click opens the context menu).
 */
export function startsPick(
  event: Pick<MouseEvent, "button" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey">,
  mac: boolean,
) {
  if (event.button !== 0 || event.altKey || event.shiftKey) return false;
  return mac ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
}

/**
 * A click on the chat row `key`. A plain click picks or drops it and makes it
 * the anchor; with `range`, every row drawn between the anchor and this one
 * is picked, as in a Finder list. With no anchor still drawn, a range click
 * is a plain one.
 */
export function pickChat(
  selection: Selection,
  key: string,
  range: boolean,
  drawn: readonly string[],
): Selection {
  const from = selection.anchor ? drawn.indexOf(selection.anchor) : -1;
  const to = drawn.indexOf(key);
  if (range && from >= 0 && to >= 0) {
    const span = drawn.slice(Math.min(from, to), Math.max(from, to) + 1);
    return { ...selection, chats: [...new Set([...selection.chats, ...span])] };
  }
  return {
    ...selection,
    chats: selection.chats.includes(key)
      ? selection.chats.filter((item) => item !== key)
      : [...selection.chats, key],
    anchor: key,
  };
}

/** Picks or drops a worktree. */
export const pickTree = (selection: Selection, id: string): Selection => ({
  ...selection,
  trees: selection.trees.includes(id)
    ? selection.trees.filter((item) => item !== id)
    : [...selection.trees, id],
});

/** Select all shown: every chat row the list draws now joins. */
export const pickAll = (selection: Selection, drawn: readonly string[]): Selection => ({
  ...selection,
  chats: [...new Set([...selection.chats, ...drawn])],
});

/**
 * What is left once an action handled `done`, keeping only picks that still
 * exist (`live`): a chat archived elsewhere, or one inside a worktree the
 * action removed, is gone. Nothing left ends the mode.
 */
export function settle(
  selection: Selection,
  done: { chats?: readonly string[]; trees?: readonly string[] },
  live: { chats: readonly string[]; trees: readonly string[] },
): Selection | null {
  const chats = selection.chats.filter(
    (key) => !done.chats?.includes(key) && live.chats.includes(key),
  );
  const trees = selection.trees.filter(
    (id) => !done.trees?.includes(id) && live.trees.includes(id),
  );
  return chats.length || trees.length ? { ...selection, chats, trees } : null;
}

const [selection, setSelection] = createSignal<Selection | null>(null);
/** The open Select chats mode, if any; one list at a time. */
export { selection, setSelection };

/** Whether the list `scope` is in Select chats mode. */
export const selecting = (scope: string) => selection()?.scope === scope;

/** The chat rows list `scope` draws now, top to bottom, by key. Rows inside
 * a collapsed project or checkout are not drawn. */
export const drawnChats = (scope: string) =>
  [
    ...document.querySelectorAll<HTMLElement>(
      `.session-row[data-pick-scope="${CSS.escape(scope)}"]`,
    ),
  ].flatMap((row) => row.dataset.pickKey ?? []);
