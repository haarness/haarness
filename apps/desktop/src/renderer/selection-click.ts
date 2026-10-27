// Tool rows open and close on a click, yet their label text can be selected.
// A mouse-up that finishes a selection also fires `click`, so a row checks,
// before it toggles, whether this gesture was really a selection: it notes the
// pointer and the selection's ends on pointer-down and compares them on click.
// The ends are compared, not the selected text, because selecting the same
// words in a second row leaves the text equal while the ends move.

/** The part of `window.getSelection()` these checks read. */
export type SelectionState = Pick<
  Selection,
  "isCollapsed" | "rangeCount" | "anchorNode" | "anchorOffset" | "focusNode" | "focusOffset"
> & { getRangeAt(index: number): Pick<Range, "intersectsNode"> };

type Ends = readonly [Node | null, number, Node | null, number];

/** What a row notes when the pointer goes down on it. */
export type SelectionMark = {
  readonly at: readonly [x: number, y: number];
  readonly ends: Ends;
  /** The row's own text was already selected. */
  readonly inside: boolean;
};

/** Pixels the pointer may wander on either axis and still be clicking, not dragging. */
const DRAG_THRESHOLD = 4;

function liveSelection(): SelectionState | null {
  return window.getSelection();
}

function endsOf(selection: SelectionState | null): Ends {
  if (!selection) return [null, 0, null, 0];
  return [selection.anchorNode, selection.anchorOffset, selection.focusNode, selection.focusOffset];
}

function selects(selection: SelectionState | null, node: Node): boolean {
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) return false;
  return selection.getRangeAt(0).intersectsNode(node);
}

export function markSelection(
  event: Pick<PointerEvent, "clientX" | "clientY"> & { currentTarget: Node },
  selection = liveSelection(),
): SelectionMark {
  return {
    at: [event.clientX, event.clientY],
    ends: endsOf(selection),
    inside: selects(selection, event.currentTarget),
  };
}

/** True when this click finished selecting text in `element`, so it must not toggle it. */
export function selectedByClick(
  element: Node,
  event: Pick<MouseEvent, "detail" | "clientX" | "clientY">,
  mark: SelectionMark | undefined,
  selection = liveSelection(),
): boolean {
  // A click with detail 0 came from the keyboard or from code: never a selection.
  if (!mark || event.detail === 0) return false;
  const [x, y] = mark.at;
  const dragged = Math.max(Math.abs(event.clientX - x), Math.abs(event.clientY - y)) > DRAG_THRESHOLD;
  // Dragging text that was already selected in the row is a text drag, even
  // when the browser clears the selection as the drag ends.
  if (!selects(selection, element)) return dragged && mark.inside;
  const now = endsOf(selection);
  return dragged || now.some((part, index) => part !== mark.ends[index]);
}
