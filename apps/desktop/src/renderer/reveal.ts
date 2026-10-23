// Bringing the chosen chat, project or tab into view in a list that scrolls.
// Both move only as far as needed and glide unless the reader asks for less
// motion; they wait a frame, so the row a choice just drew has its size.

const behavior = (): ScrollBehavior =>
  matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth";

/** Scrolls the sidebar list just far enough to show `element`. Rows keep a
 * scroll margin (shell.css) so the sticky project heading never covers one. */
export function revealInList(element: Element | null | undefined) {
  requestAnimationFrame(() => {
    if (!element?.isConnected) return;
    element.scrollIntoView({ block: "nearest", inline: "nearest", behavior: behavior() });
  });
}

/** Scrolls the tab strip holding `tab` sideways just far enough to show it.
 * Only the strip moves: `scrollIntoView` would also shift the panes around
 * it. */
export function revealInStrip(tab: HTMLElement) {
  requestAnimationFrame(() => {
    const container = tab.parentElement?.closest<HTMLElement>("[role=tablist]");
    if (!container || !tab.isConnected) return;
    const box = tab.getBoundingClientRect();
    const view = container.getBoundingClientRect();
    const delta =
      box.left < view.left ? box.left - view.left : box.right > view.right ? box.right - view.right : 0;
    if (delta) container.scrollBy({ left: delta, behavior: behavior() });
  });
}
