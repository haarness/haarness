/**
 * Where the chat and prompt keys may act. Previous and next chat (Alt and an
 * arrow by default) step the chat rows the sidebar draws, and the prompt keys
 * (Mod and an arrow) move through the open chat's prompts. Both keep away
 * from places that own those chords: the file editor moves lines with Alt and
 * an arrow, a terminal sends them to its shell, a dialog or menu has its own
 * keys, and in a draft Mod and an arrow move the caret.
 */
export type KeyPlace =
  /** No text field: the transcript, the sidebar, a button. */
  | "page"
  /** The message box with nothing typed. */
  | "composer"
  /** The message box holding a draft. */
  | "draft"
  /** Any other text field. */
  | "field"
  | "editor"
  | "terminal"
  | "overlay";

type Place = {
  closest(selector: string): unknown;
  matches(selector: string): boolean;
};

/** The place `target`, the element with the keyboard, is in. `open` says
 * whether a dialog or popover is up anywhere in the window. */
export function keyPlace(
  target: Place | null,
  draft: string,
  open: boolean,
): KeyPlace {
  if (open) return "overlay";
  if (!target) return "page";
  if (target.closest(".cm-editor")) return "editor";
  if (target.closest(".xterm, .terminal-dock")) return "terminal";
  if (target.closest(".action-menu, .select-popover, .project-popover"))
    return "overlay";
  if (target.matches("[data-composer-input]"))
    return draft.trim() ? "draft" : "composer";
  if (target.closest("input, textarea, select, [contenteditable]"))
    return "field";
  return "page";
}

/** Previous and next chat answer from the page and the message box. In a
 * draft on macOS, Option and an arrow move the caret to the paragraph's
 * start or end, so there the draft keeps them; elsewhere Alt and an arrow do
 * nothing in a draft. */
export const stepsChats = (place: KeyPlace, mac: boolean) =>
  place === "page" || place === "composer" || (place === "draft" && !mac);

/** The prompt keys answer from the page, and from the message box only
 * while it is empty. */
export const movesPrompts = (place: KeyPlace) =>
  place === "page" || place === "composer";

/** Whether a dialog or popover is open, which owns the keyboard. */
export const overlayOpen = (root: ParentNode) =>
  !!root.querySelector(
    "dialog[open], .action-menu, .select-popover, .project-popover",
  );

export type PromptMove = "previous" | "next" | "first" | "last";

/**
 * Where a prompt key lands among `count` prompts: a prompt's position, or
 * `"latest"` to follow the newest output again. `reading` is the position of
 * the prompt being read, or -1 above the first; `following` says the view is
 * at the newest output. From there, Previous goes to the last prompt; from
 * the first prompt it stays on it. Undefined when there is nowhere to go.
 */
export function promptTarget(
  move: PromptMove,
  count: number,
  reading: number,
  following: boolean,
): number | "latest" | undefined {
  if (move === "last") return "latest";
  if (!count) return undefined;
  if (move === "first") return 0;
  if (move === "previous") {
    if (following) return count - 1;
    return reading < 0 ? undefined : Math.max(0, reading - 1);
  }
  if (following) return undefined;
  return reading + 1 < count ? reading + 1 : "latest";
}

/** The position `offset` steps from `at` among `count` rows, wrapping past
 * either end. From no row (-1), a step forward lands on the first row and a
 * step back on the last. */
export function stepIndex(count: number, at: number, offset: number) {
  if (!count) return undefined;
  if (at < 0) return offset > 0 ? 0 : count - 1;
  return (((at + offset) % count) + count) % count;
}
