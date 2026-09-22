import { createSignal, onCleanup } from "solid-js";

/** Capture file drops without interfering with text drops or sidebar sorting. */
export function createFileDrop(
  receive: (files: File[]) => void,
  enabled: () => boolean = () => true,
) {
  const [dragging, setDragging] = createSignal(false);
  let depth = 0;
  const reset = () => { depth = 0; setDragging(false); };
  const ref = (element: HTMLElement) => {
    const handle = (event: DragEvent) => {
      if (!event.dataTransfer?.types.includes("Files")) return;
      event.preventDefault();
      event.stopPropagation();
      if (event.type === "dragenter") {
        depth++;
        setDragging(enabled());
      } else if (event.type === "dragleave") {
        if (--depth <= 0) reset();
      } else if (event.type === "dragover") {
        event.dataTransfer.dropEffect = enabled() ? "copy" : "none";
      } else if (event.type === "drop") {
        reset();
        const files = Array.from(event.dataTransfer.files);
        if (enabled() && files.length) receive(files);
      }
    };
    const events = ["dragenter", "dragleave", "dragover", "drop"] as const;
    for (const type of events) element.addEventListener(type, handle, true);
    window.addEventListener("dragend", reset);
    onCleanup(() => {
      for (const type of events) element.removeEventListener(type, handle, true);
      window.removeEventListener("dragend", reset);
    });
  };
  return { ref, dragging, reset };
}
