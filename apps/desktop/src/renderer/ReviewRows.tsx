import { measureRow } from "./measure-row";
import {
  For,
  Show,
  createMemo,
  createEffect,
  on,
  onCleanup,
  type Accessor,
  type JSX,
} from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";

/** A measured list sharing the review pane's scroll owner. File bodies nest
 * inside the file list, so even Show every file keeps both dimensions bounded. */
export function ReviewRows<T>(props: {
  items: T[];
  scroll: HTMLElement | undefined;
  offset?: number;
  estimate: number;
  reveal?: number;
  itemKey?: (item: T) => string;
  children: (
    item: T,
    start: Accessor<number>,
    size: Accessor<number>,
  ) => JSX.Element;
}) {
  const virtual = createVirtualizer({
    get count() {
      return props.items.length;
    },
    getScrollElement: () => props.scroll ?? null,
    // A newly visible file shares an already-scrolled pane. Starting its
    // inner virtualizer at zero would pull the whole review back to the top.
    initialOffset: () => props.scroll?.scrollTop ?? 0,
    estimateSize: () => props.estimate,
    get scrollMargin() {
      return props.offset ?? 0;
    },
    getItemKey: (index) => props.itemKey?.(props.items[index]) ?? index,
    overscan: 6,
  });
  // The outer file list owns anchoring. If nested line lists compensate the
  // same resize, both virtualizers move the shared pane and amplify the drift.
  if (props.offset !== undefined)
    virtual.shouldAdjustScrollPositionOnItemSizeChange = () => false;
  createEffect(
    on(
      () => props.reveal,
      (index) => {
        if (index === undefined || index < 0) return;
        const frame = requestAnimationFrame(() =>
          virtual.scrollToIndex(index, { align: "center" }),
        );
        onCleanup(() => cancelAnimationFrame(frame));
      },
    ),
  );
  return (
    <div class="review-rows" style={{ height: `${virtual.getTotalSize()}px` }}>
      <For each={virtual.getVirtualItems().map((item) => item.key)}>
        {(key) => {
          const item = createMemo(() =>
            virtual.getVirtualItems().find((item) => item.key === key),
          );
          return (
            <div
              class="review-row"
              data-index={item()?.index}
              ref={(element) =>
                measureRow(element, virtual.measureElement)
              }
              style={{
                // Real layout offsets keep file headers sticky in the shared
                // pane; transforms make Chromium pin them to the file bottom.
                top: `${(item()?.start ?? 0) - (props.offset ?? 0)}px`,
              }}
            >
              <Show when={item() && props.items[item()!.index]} keyed>
                {(value) =>
                  props.children(
                    value,
                    () => item()?.start ?? 0,
                    () => item()?.size ?? props.estimate,
                  )
                }
              </Show>
            </div>
          );
        }}
      </For>
    </div>
  );
}
