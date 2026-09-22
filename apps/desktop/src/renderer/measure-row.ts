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
