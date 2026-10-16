import { createSignal, onCleanup } from "solid-js";

/** Capture file drops without interfering with text drops or sidebar sorting. */
export function createFileDrop(
  receive: (files: File[], transfer: DataTransfer) => void,
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
        if (enabled() && files.length) receive(files, event.dataTransfer);
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

/**
 * Where a dropped or pasted file lives on this computer: what Electron
 * reports for it, else the `file:` URL the drop listed under its name.
 */
export function droppedPath(file: File, uris: string, reported = "") {
  if (reported) return reported;
  for (const line of uris.split(/\r?\n/)) {
    if (!line.startsWith("file://")) continue;
    try {
      const url = new URL(line.trim());
      if (url.host) continue;
      const path = decodeURIComponent(url.pathname);
      if (path.split("/").pop() === file.name) return path;
    } catch {}
  }
  return "";
}

/**
 * The text a drop of `paths` puts at the caret, between `before` and
 * `after`: a path inside `workspace` relative to it, one with a space in
 * quotes, and one space between it and any text it would touch.
 */
export function pathText(paths: string[], workspace: string, before: string, after: string) {
  const root = workspace.replace(/\/+$/, "") + "/";
  const text = paths
    .map((path) => (workspace && path.startsWith(root) ? path.slice(root.length) : path))
    .map((path) => (/\s/.test(path) ? JSON.stringify(path) : path))
    .join(" ");
  return (before && !/\s$/.test(before) ? " " : "") + text + (after && !/^\s/.test(after) ? " " : "");
}
