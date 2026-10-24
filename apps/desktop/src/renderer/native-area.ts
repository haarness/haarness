/**
 * The embedded browser is a native view composited above the window's DOM, so
 * nothing in the page can paint over it. Floating layers place themselves in
 * the space beside it; BrowserPanel freezes the browser behind a snapshot only
 * when something still has to cover it.
 */
let area: { left: number; right: number } | undefined;
const listeners = new Set<() => void>();

/** How far the browser reaches in from the window's inline end, where toasts sit. */
export function nativeEnd() {
  if (!area) return 0;
  const end = document.documentElement.dir === "rtl" ? area.right : window.innerWidth - area.left;
  return Math.max(0, Math.round(end));
}

/** Writes `--native-inline-end` for the current area and direction. */
export function applyNativeEnd() {
  const root = document.documentElement;
  if (!area) root.style.removeProperty("--native-inline-end");
  // Toasts anchor to the inline end, which is where the browser panel sits.
  else root.style.setProperty("--native-inline-end", nativeEnd() + "px");
}

/** BrowserPanel reports where its live page is drawn, or nothing when hidden. */
export function setNativeArea(rect?: DOMRect) {
  const previous = JSON.stringify(area);
  area = rect && rect.width > 0 && rect.height > 0
    ? { left: rect.left, right: rect.right }
    : undefined;
  applyNativeEnd();
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
/** How far clear of the toast column the message box must move before raised
    toasts drop back to their corner, so a divider dragged near it does not
    make them bounce. Never the other way: a box reaching the column raises
    them at once, so they never cover its Send button. */
export const TOAST_SETTLE = 32;

type Box = Pick<DOMRect, "left" | "right" | "top" | "width" | "height">;
export type ToastPlace = { clearance?: number; composer: boolean; terminal: boolean };

/**
 * How far above the window's bottom toasts sit, and whether the message box
 * raises them. Toasts clear what keeps controls under their corner: the
 * message box's content (Send, Stop, a waiting approval) while it runs under
 * the toast column, and the terminal, which spans the window's width. With
 * neither there (a side panel beside the chat, the chat hidden by an
 * expanded panel) they keep their corner. `raised` is whether the message
 * box raised them last time.
 */
export function toastPlace(anchors: { composer?: Box; terminal?: Box }, raised = false): ToastPlace {
  const width = window.innerWidth;
  const offset = TOAST_GAP + nativeEnd();
  const column = Math.max(0, Math.min(TOAST_WIDTH, width - TOAST_GAP * 2 - nativeEnd()));
  const rtl = document.documentElement.dir === "rtl";
  const [start, end] = rtl ? [offset, offset + column] : [width - offset - column, width - offset];
  const shown = (box?: Box): box is Box => !!box && box.width > 0 && box.height > 0;
  // How far a box reaches into the column from the chat's side (negative: clear of it).
  const reach = (box: Box) =>
    rtl ? (box.right <= start ? -Infinity : end - box.left) : box.left >= end ? -Infinity : box.right - start;
  const above = (box: Box) => Math.max(0, Math.round(window.innerHeight - box.top));
  const { composer, terminal } = anchors;
  const lift = shown(composer) && (reach(composer) > 0 || (raised && reach(composer) > -TOAST_SETTLE));
  const under = shown(terminal) && terminal.left < end && terminal.right > start;
  const heights = [...(lift ? [above(composer!)] : []), ...(under ? [above(terminal!)] : [])];
  return { clearance: heights.length ? Math.max(...heights) : undefined, composer: lift, terminal: under };
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

/** The box of a dialog, which a layer opened inside it stays in where it fits. */
export type Frame = Pick<DOMRect, "left" | "right" | "top" | "bottom">;

/** The box of the dialog `element` is in (Settings, a form), if any. */
export function dialogFrame(element: Element): Frame | undefined {
  const box = element.closest('dialog, [role="dialog"]')?.getBoundingClientRect();
  return box && box.width > 0 && box.height > 0 ? box : undefined;
}

/**
 * Horizontal placement for a layer opened from `anchor`: beside the browser
 * when the anchor is beside it and the layer fits there, else the whole window.
 * The layer keeps `preferred` width where it can and shrinks to `minimum`.
 * Opened in a dialog (`frame`), it keeps 8px inside the dialog's box when it
 * fits there, rather than hanging over what the dialog covers; one that
 * cannot start where its anchor starts then ends where it ends, so an edge
 * still lines up with the control.
 */
export function floatingBox(anchor: DOMRect, preferred: number, minimum = preferred, frame?: Frame) {
  const gutter = 8;
  const span = floatingSpan(anchor, minimum);
  const inside = frame && { start: Math.max(span.start, frame.left), end: Math.min(span.end, frame.right) };
  const framed = !!inside && inside.end - inside.start - gutter * 2 >= minimum;
  const { start, end } = framed ? inside : span;
  const width = Math.max(0, Math.min(preferred, end - start - gutter * 2));
  const ended = anchor.right - width;
  const left =
    framed && anchor.left + width > end - gutter && ended >= start + gutter ? ended : anchor.left;
  return {
    width,
    left: Math.max(start + gutter, Math.min(left, end - width - gutter)),
  };
}

/** How short a popover whose `list` scrolls may get: five of its rows. */
export function shortestList(popup: HTMLElement, list: HTMLElement) {
  const height = popup.getBoundingClientRect().height;
  const row = list.querySelector('[role="option"]')?.getBoundingClientRect().height ?? 0;
  return Math.min(height, height - list.getBoundingClientRect().height + 5 * row);
}

/**
 * Where a layer `height` tall, opened `gap` below or above `anchor`, sits
 * inside a dialog's box, 8px in from its edges: below when it fits there,
 * else above. A list that scrolls can be shortened to the larger room, down to
 * `shortest`; the result then says the `height` to give it. Undefined when it
 * does not fit: the layer is placed against the window, as outside a dialog.
 */
export function frameTop(anchor: DOMRect, height: number, gap: number, frame?: Frame, shortest = height) {
  if (!frame) return undefined;
  const gutter = 8;
  const below = frame.bottom - gutter - (anchor.bottom + gap);
  const above = anchor.top - gap - (frame.top + gutter);
  if (height <= below) return { top: anchor.bottom + gap };
  if (height <= above) return { top: anchor.top - gap - height };
  const room = Math.floor(Math.max(below, above));
  if (room < shortest) return undefined;
  return { top: below >= above ? anchor.bottom + gap : anchor.top - gap - room, height: room };
}
