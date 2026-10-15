import { t } from "./i18n";
import { For, Index, Show, createEffect, createMemo, createSignal, onCleanup, onMount, untrack } from "solid-js";
import { EDGE, PAD, ROW, fits, fold, rowAt, sliceOf, slices, unfold, visible } from "./outline-layout";
import "./outline.css";

/** Width of the hit area over the dashes at rest, from the leading edge. */
const LANE = 32;
/** Rows drawn beyond the panel's edges while it scrolls. */
const OVERSCAN = 4;
/** Grace before a pointer that left the panel closes it. */
const LINGER = 160;

export type OutlinePrompt = { id: string; title: string };

const sameList = (a: readonly number[], b: readonly number[]) =>
  a.length === b.length && a.every((item, i) => item === b[i]);

/**
 * The conversation's prompts as a short stack of dashes at the top of the
 * transcript's leading edge, like a table of contents. At rest the dashes are
 * dim and the prompt being read is longer and brighter; past `FOLD_MAX`
 * prompts each dash stands for an even slice. Hovering opens a panel with one
 * row per prompt and its first line; the row under the pointer is washed, and
 * a click jumps there. The panel shows at most `ROWS_MAX` rows and draws only
 * those in view. The lane sits over the transcript, not inside it, and hands
 * wheel input it does not use back to the conversation.
 */
export function ConversationOutline(props: {
  prompts: () => readonly OutlinePrompt[];
  reading: () => string | undefined;
  /** Height of the transcript's viewport. */
  height: () => number;
  onJump: (index: number) => void;
  onWheel: (event: WheelEvent) => void;
}) {
  let lane: HTMLElement | undefined;
  const count = () => props.prompts().length;
  const order = createMemo(() => new Map(props.prompts().map((item, i) => [item.id, i] as const)));
  const starts = createMemo(() => slices(count()), [], { equals: sameList });
  const folded = createMemo(() => fold(starts().length, props.height()));
  const shown = createMemo(() => fits(props.height(), folded().top));
  const current = createMemo(() => {
    const id = props.reading();
    return id ? (order().get(id) ?? -1) : -1;
  });

  const [open, setOpen] = createSignal<{ anchor: number; y: number }>();
  const [hot, setHot] = createSignal(-1);
  const [scrolled, setScrolled] = createSignal(0);
  const box = createMemo(() => {
    const value = open();
    return value && shown() && unfold(count(), value.anchor, value.y, props.height(), folded().top);
  });
  // Only the rows in view, plus a few either side, are in the DOM.
  const rows = createMemo(
    () => {
      const value = box();
      if (!value) return [];
      const [first, last] = visible(count(), value.size, scrolled());
      const start = Math.max(0, first - OVERSCAN);
      const end = Math.min(count() - 1, last + OVERSCAN);
      return Array.from({ length: Math.max(end - start + 1, 0) }, (_, k) => start + k);
    },
    [],
    { equals: sameList },
  );

  // Where the pointer last was, so scrolling the panel moves the highlight
  // to the row now under a still pointer.
  let pointer = 0;
  let linger: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(linger));
  const close = () => {
    clearTimeout(linger);
    setOpen(undefined);
    setHot(-1);
  };
  const leave = () => {
    clearTimeout(linger);
    linger = setTimeout(close, LINGER);
  };
  // A lane that became too short closes the panel rather than keeping it
  // half-open, so the next pointer move after it grows opens it afresh.
  createEffect(() => {
    if (!shown() && untrack(open)) close();
  });
  const local = (event: MouseEvent) => event.clientY - (lane?.getBoundingClientRect().top ?? 0);
  // A list longer than the panel starts scrolled to the dash the pointer
  // opened on: the prompt being read when that dash's slice holds it.
  const openAt = (y: number) => {
    if (!shown()) return;
    const list = starts();
    const pitch = folded().pitch;
    const slot = pitch > 0 ? Math.max(0, Math.min(list.length - 1, Math.round((y - folded().top) / pitch))) : 0;
    const reading = current();
    const anchor = reading >= 0 && sliceOf(list, reading) === slot ? reading : (list[slot] ?? 0);
    const value = unfold(count(), anchor, y, props.height(), folded().top);
    setScrolled(value.scroll);
    setOpen({ anchor, y });
    setHot(rowAt(count(), y, value.top, value.scroll));
  };
  // The panel stays open, so the user can hop between prompts; it closes
  // when the pointer leaves.
  const jump = (index: number) => props.onJump(index);

  return (
    <nav
      ref={lane}
      class="outline"
      hidden={!shown()}
      data-state={box() ? "open" : "folded"}
      aria-label={t("Chat outline")}
    >
      <Index each={starts()}>
        {(start, slot) => {
          // A dash is being read when its slice holds the prompt being read.
          const reading = () => current() >= 0 && sliceOf(starts(), current()) === slot;
          return (
            <button
              type="button"
              tabIndex={-1}
              class="outline-dash"
              data-prompt-id={props.prompts()[start()]?.id}
              aria-label={t("Jump to prompt {index}", { index: start() + 1 })}
              aria-current={reading() ? "location" : undefined}
              style={{ top: `${folded().top + slot * folded().pitch}px`, "inset-inline-start": `${EDGE}px` }}
              // Pointer input lands on the lane, so this runs for assistive technology.
              onClick={() => jump(start())}
            >
              <span class="outline-mark" classList={{ reading: reading() }} aria-hidden="true" />
            </button>
          );
        }}
      </Index>
      <div
        class="outline-lane"
        style={{
          top: `${folded().top - 14}px`,
          // Never past the lane's bottom edge.
          height: `${Math.min(folded().pitch * Math.max(starts().length - 1, 0) + 28, props.height() - folded().top + 14)}px`,
          width: `${LANE}px`,
        }}
        onPointerMove={(event) => {
          pointer = local(event);
          clearTimeout(linger);
          if (!open()) openAt(pointer);
        }}
        onPointerLeave={leave}
        onWheel={(event) => props.onWheel(event)}
      />
      <Show when={box()}>
        {(value) => {
          let panel: HTMLDivElement | undefined;
          // Start scrolled so the row the pointer opened on sits under it.
          onMount(() => {
            if (panel) panel.scrollTop = value().scroll;
          });
          return (
            <div
              ref={panel}
              class="outline-panel"
              style={{
                top: `${value().top}px`,
                // Plus the 1px border at either end, so a list that fits does not scroll.
                height: `${value().size + 2}px`,
                "inset-inline-start": `${EDGE - 6}px`,
              }}
              onScroll={(event) => {
                setScrolled(event.currentTarget.scrollTop);
                setHot(rowAt(count(), pointer, value().top, event.currentTarget.scrollTop));
              }}
              onPointerMove={(event) => (pointer = local(event))}
              onPointerEnter={() => clearTimeout(linger)}
              onPointerLeave={leave}
              onWheel={(event) => {
                // A panel with nothing to scroll hands the wheel to the conversation.
                if (event.currentTarget.scrollHeight <= event.currentTarget.clientHeight) props.onWheel(event);
              }}
            >
              <div class="outline-rows" style={{ height: `${count() * ROW + PAD * 2}px` }}>
                <For each={rows()}>
                  {(i) => (
                    <Show when={props.prompts()[i]}>
                      {(prompt) => (
                        <button
                          type="button"
                          tabIndex={-1}
                          class="outline-row"
                          data-prompt-id={prompt().id}
                          data-state={hot() === i ? "hot" : undefined}
                          aria-current={current() === i ? "location" : undefined}
                          style={{ top: `${PAD + i * ROW + 2}px`, height: `${ROW - 4}px` }}
                          onPointerEnter={() => setHot(i)}
                          onClick={() => jump(i)}
                        >
                          <span class="outline-mark" classList={{ reading: current() === i }} aria-hidden="true" />
                          {/* User text keeps its own direction, so an English prompt
                              truncates at its end in Arabic and the reverse. */}
                          <span class="outline-title" dir="auto">{prompt().title}</span>
                        </button>
                      )}
                    </Show>
                  )}
                </For>
              </div>
            </div>
          );
        }}
      </Show>
    </nav>
  );
}
