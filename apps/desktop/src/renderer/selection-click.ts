// Adapted from Upstream (MIT). See public/upstream-preview-license.txt.
//
// A row whose label text can be selected (a tool row's <summary>) still
// toggles on a plain click, but a click that ends a text selection is not an
// activation. The row records where the selection stood when the pointer went
// down; on click it ignores the press when the selection now covers the row
// and this gesture made it: the selection moved, or the pointer dragged.
// Comparing boundary points rather than the selected string matters:
// selecting `src/a.ts` in one row and then the same `src/a.ts` in another
// leaves the string unchanged.

type Point = { node: Node | null; offset: number };

export type SelectionMark = { x: number; y: number; anchor: Point; focus: Point; held: boolean };

/** What these checks read from `window.getSelection()`. */
export type SelectionState = Pick<
  Selection,
  "isCollapsed" | "rangeCount" | "anchorNode" | "anchorOffset" | "focusNode" | "focusOffset"
> & { getRangeAt(index: number): Pick<Range, "intersectsNode"> };

/** Pointer travel that counts as a drag. Below it a press is a click, even on a shaky trackpad. */
const SLOP = 4;

const current = (): SelectionState | null => window.getSelection();

const covers = (selection: SelectionState | null, element: Node) =>
  !!selection &&
  !selection.isCollapsed &&
  selection.rangeCount > 0 &&
  selection.getRangeAt(0).intersectsNode(element);

export function markSelection(
  event: Pick<PointerEvent, "clientX" | "clientY"> & { currentTarget: Node },
  selection = current(),
): SelectionMark {
  return {
    x: event.clientX,
    y: event.clientY,
    anchor: { node: selection?.anchorNode ?? null, offset: selection?.anchorOffset ?? 0 },
    focus: { node: selection?.focusNode ?? null, offset: selection?.focusOffset ?? 0 },
    held: covers(selection, event.currentTarget),
  };
}

/** True when this click ended a text selection on `element` rather than activating it. */
export function selectedByClick(
  element: Node,
  event: Pick<MouseEvent, "detail" | "clientX" | "clientY">,
  mark: SelectionMark | undefined,
  selection = current(),
) {
  // detail 0: Enter, Space or a programmatic click(), never a selection gesture.
  if (event.detail === 0 || !mark) return false;
  const moved = Math.abs(event.clientX - mark.x) > SLOP || Math.abs(event.clientY - mark.y) > SLOP;
  // Dragging text that was already selected here is a text drag, even when the
  // browser drops the selection on release.
  if (!covers(selection, element)) return moved && mark.held;
  const changed =
    selection!.anchorNode !== mark.anchor.node ||
    selection!.anchorOffset !== mark.anchor.offset ||
    selection!.focusNode !== mark.focus.node ||
    selection!.focusOffset !== mark.focus.offset;
  return moved || changed;
}
