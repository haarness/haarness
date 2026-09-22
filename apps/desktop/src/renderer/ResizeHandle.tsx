import { createSignal } from "solid-js";

export function ResizeHandle(props: {
  label: string;
  orientation?: "horizontal" | "vertical";
  value: number;
  min: number;
  max: number;
  onDelta: (delta: number) => void;
  onReset: () => void;
}) {
  const [dragging, setDragging] = createSignal(false);
  let last = 0;
  const vertical = () => props.orientation !== "horizontal";
  return (
    <div
      class="resize-handle"
      classList={{ dragging: dragging(), horizontal: !vertical() }}
      role="separator"
      aria-label={props.label}
      aria-orientation={vertical() ? "vertical" : "horizontal"}
      aria-valuenow={Math.round(props.value)}
      aria-valuemin={props.min}
      aria-valuemax={props.max}
      tabIndex={0}
      onDblClick={props.onReset}
      onKeyDown={(event) => {
        const backwards = vertical() ? "ArrowLeft" : "ArrowUp";
        const forwards = vertical() ? "ArrowRight" : "ArrowDown";
        if (event.key === backwards || event.key === forwards) {
          event.preventDefault();
          props.onDelta(
            (event.key === forwards ? 1 : -1) * (event.shiftKey ? 50 : 20),
          );
        } else if (event.key === "Home") {
          event.preventDefault();
          props.onReset();
        }
      }}
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        event.preventDefault();
        event.currentTarget.setPointerCapture(event.pointerId);
        last = vertical() ? event.clientX : event.clientY;
        setDragging(true);
      }}
      onPointerMove={(event) => {
        if (!dragging()) return;
        const next = vertical() ? event.clientX : event.clientY;
        props.onDelta(next - last);
        last = next;
      }}
      onPointerUp={() => setDragging(false)}
      onPointerCancel={() => setDragging(false)}
      onLostPointerCapture={() => setDragging(false)}
    />
  );
}
