import { language, t } from "./i18n";
import { createEffect, on, onCleanup, onMount, Show } from "solid-js";
import { Markdown } from "./Markdown";
import { Icon } from "./icons";
import { isOpen, setOpen } from "./disclosure";
import { thoughtDuration } from "./thinking";

/**
 * A finished thought in Compact: one step row, "Thought for 12s", on the
 * same rail as the tools, that opens in place to the text. While the model
 * thinks, the activity line ticks through its newest line instead, so the
 * transcript never grows or jumps with a thought.
 */
export function ThoughtRow(props: {
  id: string;
  text: string;
  durationMs?: number | null;
  /** The thought is the current Find match: it opens to show it. */
  matched?: boolean;
}) {
  const key = () => props.id + ":thought";
  createEffect(() => {
    if (props.matched) setOpen(key(), true);
  });
  const label = () =>
    props.durationMs == null
      ? t("Thought")
      : t("Thought for {duration}", { duration: thoughtDuration(props.durationMs, language()) });
  return (
    <details
      class="tool-call thought"
      data-status="done"
      open={isOpen(key())}
      onToggle={(event) => setOpen(key(), event.currentTarget.open)}
    >
      <summary>
        <span class="tool-mark" aria-hidden="true">
          <Icon name="reasoning" size="sm" />
        </span>
        <span class="tool-label">
          <strong>{label()}</strong>
        </span>
        <Icon name="chevron" size="xs" class="tool-chevron" />
      </summary>
      <Show when={isOpen(key())}>
        <div class="tool-body thought-body">
          <Markdown text={props.text} />
        </div>
      </Show>
    </details>
  );
}

/**
 * Compact's ticker: the newest line of thought after "Thinking", on the
 * activity row. When the line is longer than the row, its start gives way
 * to "…" so the words just thought stay in view, whichever way the
 * interface or the thought runs (the TUI's `tail_line`).
 */
export function ThoughtTicker(props: { text: string }) {
  let box!: HTMLSpanElement;
  let line!: HTMLSpanElement;
  const segmenter = new Intl.Segmenter(undefined, { granularity: "grapheme" });
  const fit = () => {
    const text = props.text;
    line.textContent = text;
    if (line.scrollWidth <= box.clientWidth) return;
    const graphemes = Array.from(segmenter.segment(text), (part) => part.segment);
    // The most graphemes from the end that fit after the ellipsis.
    let low = 0;
    let high = graphemes.length;
    while (low < high) {
      const middle = Math.ceil((low + high) / 2);
      line.textContent = "…" + graphemes.slice(graphemes.length - middle).join("");
      if (line.scrollWidth <= box.clientWidth) low = middle;
      else high = middle - 1;
    }
    line.textContent = "…" + graphemes.slice(graphemes.length - low).join("");
  };
  createEffect(on(() => props.text, () => fit()));
  onMount(() => {
    const observer = new ResizeObserver(() => fit());
    observer.observe(box);
    onCleanup(() => observer.disconnect());
  });
  return (
    <span ref={box} class="activity-thought" aria-hidden="true">
      <span ref={line} dir="auto" />
    </span>
  );
}
