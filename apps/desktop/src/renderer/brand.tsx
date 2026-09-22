/*
 * Brand artwork: the Haarness mark and wordmark.
 *
 * Both are the "Harness" drawings from `brand/` — two posts joined by a strap
 * that overhangs them. The mark is the H on its own; the wordmark is the same
 * H redrawn at the interface face's own cap height, stem weight and side
 * bearings so it stands in for the word's first letter instead of sitting
 * beside a letter it would only repeat.
 *
 * Both inherit `currentColor`, so a surface sets the colour by setting its own.
 * Functional icons live in icons.tsx; this file is the brand, and nothing else
 * belongs in it.
 */
import { Show } from "solid-js";

/** The square mark. `period` adds the full stop, which the brand omits. */
export function Mark(props: { class?: string; period?: boolean; label?: string }) {
  return (
    <svg
      class={"brand-mark" + (props.class ? " " + props.class : "")}
      viewBox="0 0 64 64"
      fill="currentColor"
      role={props.label ? "img" : undefined}
      aria-label={props.label}
      aria-hidden={props.label ? undefined : "true"}
    >
      <rect x="17" y="14" width="8" height="36" rx="2" />
      <rect x="39" y="14" width="8" height="36" rx="2" />
      <rect x="13.5" y="27" width="37" height="8" rx="4" />
      <Show when={props.period}>
        <circle cx="55" cy="46" r="4" />
      </Show>
    </svg>
  );
}

/*
 * The wordmark. Its box is measured in the same units as the type: the cap is
 * 70.459 tall at font-size 100, so the drawing scales off the element's own
 * font-size and lines up with text set beside it. The H keeps the square
 * mark's own stem and overhang rather than the typeface's, so it reads as the
 * logo instead of as a slightly heavy letter.
 */
const STACK =
  '-apple-system, BlinkMacSystemFont, "Segoe UI Variable Text", "Segoe UI", Inter, Roboto, "Helvetica Neue", Arial, sans-serif';

export function Wordmark(props: { class?: string }) {
  return (
    <svg
      class={"brand-wordmark" + (props.class ? " " + props.class : "")}
      viewBox="-6.665 -6 436.758 83.5"
      fill="currentColor"
      role="img"
      aria-label="Haarness"
    >
      <rect x="6.185" y="0" width="15.6576" height="70.459" rx="3.9144" />
      <rect x="50.0974" y="0" width="15.6576" height="70.459" rx="3.9144" />
      <rect x="-0.6652" y="25.5218" width="73.2704" height="15.6576" rx="7.8288" />
      <text
        x="75.774"
        y="70.459"
        font-family={STACK}
        font-size="100"
        font-weight="600"
        letter-spacing="-3"
      >
        aarness.
      </text>
    </svg>
  );
}
