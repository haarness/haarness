import { onCleanup } from "solid-js";
import { applyNativeEnd, onNativeArea, toastPlace } from "./native-area";

/**
 * Keeps toasts (overlay.css `.notifications`) clear of what sits under their
 * corner: the message box and the terminal register themselves, and every
 * change of their size or place, of the window, the browser's drawn area or
 * the interface direction places the toasts again (native-area.ts decides
 * where). A toast glides when what it clears changes (a panel, the terminal
 * or the message box coming or going) and follows a growing message box or a
 * dragged divider at once, so it never trails behind or bounces.
 */
type Kind = "composer" | "terminal";
const elements = new Map<Kind, HTMLElement>();
let observer: ResizeObserver | undefined;
let raised = false;
let placed: string | undefined;
let glide: ReturnType<typeof setTimeout> | undefined;

/** The message box counts by its content, without the padding that centres it. */
function box(kind: Kind, element: HTMLElement) {
  const rect = element.getBoundingClientRect();
  if (kind === "terminal") return rect;
  const style = getComputedStyle(element);
  const left = rect.left + parseFloat(style.paddingLeft);
  const right = rect.right - parseFloat(style.paddingRight);
  return { left, right, top: rect.top, width: right - left, height: rect.height };
}

function place() {
  const at = (kind: Kind) => {
    const element = elements.get(kind);
    return element?.isConnected ? box(kind, element) : undefined;
  };
  const next = toastPlace({ composer: at("composer"), terminal: at("terminal") }, raised);
  raised = next.composer;
  const root = document.documentElement;
  const key = `${next.composer}:${next.terminal}`;
  if (placed !== undefined && key !== placed) {
    // Set before the new place, so the move itself is the one that glides.
    root.dataset.toastGlide = "";
    clearTimeout(glide);
    glide = setTimeout(() => delete root.dataset.toastGlide, 400);
  }
  placed = key;
  if (next.clearance === undefined) root.style.removeProperty("--toast-clearance");
  else root.style.setProperty("--toast-clearance", next.clearance + "px");
}

function observe() {
  if (!observer) {
    observer = new ResizeObserver(place);
    window.addEventListener("resize", place);
    onNativeArea(place);
    new MutationObserver(() => {
      applyNativeEnd();
      place();
    }).observe(document.documentElement, { attributes: true, attributeFilter: ["dir"] });
  }
  observer.disconnect();
  for (const element of elements.values()) {
    // A padded box takes up a change of width, so watch the border box; the
    // element around it says when it moves without resizing.
    observer.observe(element, { box: "border-box" });
    if (element.parentElement) observer.observe(element.parentElement);
  }
  place();
}

/** A ref that keeps toasts clear of this element for as long as it is shown. */
export function toastAnchor(kind: Kind) {
  return (element: HTMLElement) => {
    elements.set(kind, element);
    // The element is attached to the page only after its ref runs.
    queueMicrotask(() => elements.get(kind) === element && observe());
    onCleanup(() => {
      if (elements.get(kind) !== element) return;
      elements.delete(kind);
      observe();
    });
  };
}
