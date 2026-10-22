/**
 * The embedded browser is a native view composited above the window's DOM, so
 * nothing in the page can paint over it. Floating layers place themselves in
 * the space beside it; BrowserPanel freezes the browser behind a snapshot only
 * when something still has to cover it.
 */
let area: { left: number; right: number } | undefined;
/** How far the browser reaches in from the window's inline end, where toasts sit. */
let nativeEnd = 0;
const listeners = new Set<() => void>();

/** BrowserPanel reports where its live page is drawn, or nothing when hidden. */
export function setNativeArea(rect?: DOMRect) {
  const root = document.documentElement;
  const previous = JSON.stringify(area);
  area = rect && rect.width > 0 && rect.height > 0
    ? { left: rect.left, right: rect.right }
    : undefined;
  // Toasts anchor to the inline end, which is where the browser panel sits.
  nativeEnd = area
    ? Math.max(0, Math.round(root.dir === "rtl" ? area.right : window.innerWidth - area.left))
    : 0;
  if (!area) root.style.removeProperty("--native-inline-end");
  else root.style.setProperty("--native-inline-end", nativeEnd + "px");
  if (JSON.stringify(area) !== previous) listeners.forEach((listener) => listener());
}

/** Calls `listener` whenever the browser's drawn area changes; the result stops it. */
export function onNativeArea(listener: () => void) {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

/** The toast column as overlay.css draws `.notifications`: up to 400px wide,
    16px (--s4) in from the inline end, beside the browser when it is drawn. */
const TOAST_WIDTH = 400;
const TOAST_GAP = 16;

/**
 * How far above the window's bottom toasts sit: over the composer's dock
 * (Send, Stop, a waiting approval) when the dock runs under the toast column,
 * and nothing otherwise, so toasts keep their corner beside a side panel and
 * while the chat is hidden. `dock` is the dock's content box.
 */
export function toastClearance(dock: Pick<DOMRect, "left" | "right" | "top" | "width" | "height">) {
  if (!(dock.width > 0) || !(dock.height > 0)) return undefined;
  const width = window.innerWidth;
  const offset = TOAST_GAP + nativeEnd;
  const column = Math.max(0, Math.min(TOAST_WIDTH, width - TOAST_GAP * 2 - nativeEnd));
  const [start, end] = document.documentElement.dir === "rtl"
    ? [offset, offset + column]
    : [width - offset - column, width - offset];
  if (dock.right <= start || dock.left >= end) return undefined;
  return Math.max(0, Math.round(window.innerHeight - dock.top));
}

/** The horizontal span beside the browser on `anchor`'s side, or the window. */
export function floatingSpan(anchor: DOMRect, minimum = 0) {
  const gutter = 8;
  const beside = !area
    ? undefined
    : anchor.right <= area.left + 1
      ? { start: 0, end: area.left }
      : anchor.left >= area.right - 1
        ? { start: area.right, end: window.innerWidth }
        : undefined;
  return beside && beside.end - beside.start - gutter * 2 >= minimum
    ? beside
    : { start: 0, end: window.innerWidth };
}

/**
 * Horizontal placement for a layer opened from `anchor`: beside the browser
 * when the anchor is beside it and the layer fits there, else the whole window.
 * The layer keeps `preferred` width where it can and shrinks to `minimum`.
 */
export function floatingBox(anchor: DOMRect, preferred: number, minimum = preferred) {
  const gutter = 8;
  const { start, end } = floatingSpan(anchor, minimum);
  const width = Math.max(0, Math.min(preferred, end - start - gutter * 2));
  return {
    width,
    left: Math.max(start + gutter, Math.min(anchor.left, end - width - gutter)),
  };
}
