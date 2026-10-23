import { onCleanup } from "solid-js";

/** Solid disposes owners before detaching their DOM. Sweep measurements after
 * removal so virtualizers release both observers and cached row elements. */
export function measureRow(
  element: HTMLElement,
  measure: (element: HTMLElement | null) => void,
) {
  queueMicrotask(() => {
    if (element.isConnected) measure(element);
  });
  onCleanup(() => queueMicrotask(() => measure(null)));
}

/**
 * Whether the transcript's virtualizer moves the view when a row changes
 * size, so what is read stays in place. A row measured for the first time
 * that starts above the view does, as TanStack decides; a row measured again
 * does only when it lies wholly above the view (an image coming in above
 * what is read), now also while scrolling up, which TanStack's own rule
 * leaves out and so let the text jump. A row reaching into the view grows
 * below its top, where the reader is (a reply being written), so it does not.
 */
export function keepsRowsAbove<K>(
  item: { key: K; start: number; end: number },
  instance: { scrollOffset: number | null; scrollAdjustments: number; itemSizeCache: ReadonlyMap<K, number> },
) {
  const offset = (instance.scrollOffset ?? 0) + instance.scrollAdjustments;
  return instance.itemSizeCache.has(item.key) ? item.end <= offset : item.start < offset;
}
