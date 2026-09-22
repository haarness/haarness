import { createEffect, onCleanup, onMount, type ParentProps } from "solid-js";

/** Keep one phase through state refreshes, virtual-row remounts and pauses. */
export function Shimmer(
  props: ParentProps<{ active?: boolean; class?: string }>,
) {
  let element!: HTMLSpanElement;
  function synchronize() {
    for (const animation of element.getAnimations()) {
      if (
        animation instanceof CSSAnimation &&
        animation.animationName === "activity-shimmer"
      ) {
        // CSS normally starts at insertion time. Use the document timeline's
        // origin so a recreated or resumed label cannot restart the sweep.
        if (animation.startTime !== 0) animation.startTime = 0;
      }
    }
  }
  // Synchronize in the render's effect flush, before its first paint. Waiting
  // only for animationstart can briefly show phase zero before seeking forward.
  createEffect(() => {
    if (props.active ?? true) synchronize();
  });
  onMount(() => {
    const motion = matchMedia("(prefers-reduced-motion: reduce)");
    motion.addEventListener("change", synchronize);
    onCleanup(() => motion.removeEventListener("change", synchronize));
  });
  return (
    <span
      ref={element}
      classList={{
        [props.class ?? ""]: !!props.class,
        "activity-shimmer": props.active ?? true,
      }}
      onAnimationStart={(event) => {
        if (event.animationName === "activity-shimmer") synchronize();
      }}
    >
      {props.children}
    </span>
  );
}
