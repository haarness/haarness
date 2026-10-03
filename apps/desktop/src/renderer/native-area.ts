/**
 * The embedded browser is a native view composited above the window's DOM, so
 * nothing in the page can paint over it. Floating layers place themselves in
 * the space beside it; BrowserPanel freezes the browser behind a snapshot only
 * when something still has to cover it.
 */
let area: { left: number; right: number } | undefined;

/** BrowserPanel reports where its live page is drawn, or nothing when hidden. */
export function setNativeArea(rect?: DOMRect) {
  const root = document.documentElement;
  area = rect && rect.width > 0 && rect.height > 0
    ? { left: rect.left, right: rect.right }
    : undefined;
  if (!area) return root.style.removeProperty("--native-inline-end");
  // Toasts anchor to the inline end, which is where the browser panel sits.
  const end = root.dir === "rtl" ? area.right : window.innerWidth - area.left;
  root.style.setProperty("--native-inline-end", Math.max(0, Math.round(end)) + "px");
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
