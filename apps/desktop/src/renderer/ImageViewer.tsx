import { t } from "./i18n";
import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { Button } from "./ui";

/*
 * A conversation's images open full size here: an attachment, and an image
 * in a reply, thinking, a side chat or a read tool's preview that is shown
 * smaller than it is. It shows the address the image already shows (a data
 * or blob URL), so opening it loads nothing new, scaled to fill the window.
 * Esc, a click outside the image or Close shuts it, and the keyboard is back
 * on the image that opened it. The arrow keys step through the other images
 * of the same conversation on screen.
 */

/** The images the viewer opens: ones the page already holds, never an address it would fetch. */
const holds = (src: string | null | undefined) => !!src && /^(?:data:image\/|blob:)/i.test(src.trim());

const picture = (element: Element) =>
  element instanceof HTMLImageElement ? element : element.querySelector("img");

/** Whether an image is shown smaller than it is, so opening it shows more. */
const shrunk = (image: HTMLImageElement) =>
  image.complete &&
  image.naturalWidth > 0 &&
  (image.clientWidth + 1 < image.naturalWidth || image.clientHeight + 1 < image.naturalHeight);

/** What marks an image that opens: focusable, and a button to assistive technology. */
const MARKS = ["data-image-open", "tabindex", "role", "aria-haspopup"];

/**
 * Marks each image under `root` that opens larger, and unmarks one that no
 * longer would: an image the page holds, shown smaller than it is (a zoom
 * cursor says so), outside any link, which keeps its own click. Run again
 * when an image loads and when `root` changes size (`watchOpenImages`).
 */
export function markOpenImages(root: ParentNode, unnamed: string) {
  for (const image of Array.from(root.querySelectorAll("img"))) {
    const open = holds(image.getAttribute("src")) && !image.closest("a") && shrunk(image);
    if (open === image.hasAttribute("data-image-open")) continue;
    if (open) {
      image.setAttribute("data-image-open", "");
      image.setAttribute("tabindex", "0");
      image.setAttribute("role", "button");
      image.setAttribute("aria-haspopup", "dialog");
      if (!image.getAttribute("alt")) image.setAttribute("aria-label", unnamed);
    } else {
      for (const mark of MARKS) image.removeAttribute(mark);
      if (!image.getAttribute("alt")) image.removeAttribute("aria-label");
    }
  }
}

/** Whether an attribute is one `markOpenImages` keeps on an image, which Markdown's patches leave to it. */
export const openMark = (element: Element, attribute: string) =>
  element.hasAttribute("data-image-open") && (MARKS.includes(attribute) || attribute === "aria-label");

/**
 * Keeps the marks under `root` right as its images load and as it is
 * resized, and opens a marked image on a click or on Enter or Space. A
 * modified click is the person's own (a selection). Gives back `refresh`,
 * for after `root` is drawn anew, and the cleanup.
 */
export function watchOpenImages(root: HTMLElement, unnamed: () => string) {
  const refresh = () => markOpenImages(root, unnamed());
  const resized = new ResizeObserver(refresh);
  resized.observe(root);
  root.addEventListener("load", refresh, true);
  const open = (event: MouseEvent | KeyboardEvent) => {
    if (event instanceof KeyboardEvent && ((event.key !== "Enter" && event.key !== " ") || event.repeat)) return;
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const image = (event.target as HTMLElement).closest?.<HTMLElement>("[data-image-open]");
    if (!image || !root.contains(image) || image.closest("a")) return;
    event.preventDefault();
    event.stopPropagation();
    openImage(image);
  };
  root.addEventListener("click", open);
  root.addEventListener("keydown", open);
  return {
    refresh,
    dispose() {
      resized.disconnect();
      root.removeEventListener("load", refresh, true);
      root.removeEventListener("click", open);
      root.removeEventListener("keydown", open);
    },
  };
}

type Shown = { src: string; alt: string; width: number; height: number };
type Viewing = { images: Shown[]; index: number; opener: HTMLElement };
const [viewing, setViewing] = createSignal<Viewing>();

/**
 * Opens `from`, an image marked `data-image-open` or a button holding one,
 * with the other such images of its conversation (or side chat) to step to.
 */
export function openImage(from: HTMLElement) {
  const group = from.closest(".transcript, .side-chat-scroll") ?? from.closest(".markdown") ?? from;
  const elements = Array.from(group.querySelectorAll<HTMLElement>("[data-image-open]")).filter((element) =>
    holds(picture(element)?.getAttribute("src")),
  );
  if (!elements.includes(from)) {
    if (!holds(picture(from)?.getAttribute("src"))) return;
    elements.splice(0, elements.length, from);
  }
  setViewing({
    images: elements.map((element) => {
      const image = picture(element)!;
      return {
        src: image.getAttribute("src")!,
        alt: image.getAttribute("alt") || "",
        width: image.naturalWidth,
        height: image.naturalHeight,
      };
    }),
    index: elements.indexOf(from),
    opener: from,
  });
}

export function ImageViewer() {
  return (
    <Show when={viewing()} keyed>
      {(current) => {
        let dialog!: HTMLDialogElement;
        let image!: HTMLImageElement;
        const [index, setIndex] = createSignal(current.index);
        const shown = () => current.images[index()];
        // The room the window leaves inside the dialog's padding.
        const [room, setRoom] = createSignal({ width: 0, height: 0 });
        const measure = () => {
          const style = getComputedStyle(dialog);
          setRoom({
            width: dialog.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight),
            height: dialog.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom),
          });
        };
        /** The image scaled to fill that room, its ratio kept; a smaller one is scaled up to it. */
        const size = () => {
          const { width, height } = shown();
          if (!width || !height || !room().width) return {};
          const scale = Math.min(room().width / width, room().height / height);
          return { width: `${Math.floor(width * scale)}px`, height: `${Math.floor(height * scale)}px` };
        };
        const close = () => {
          if (viewing() !== current) return;
          dialog.close();
          setViewing(undefined);
          // Shown from code, the dialog has no trigger to hand the keyboard back to.
          if (current.opener.isConnected) current.opener.focus({ preventScroll: true });
        };
        // Only a whole click outside the image closes it, never a drag that ends there.
        let outside = false;
        onMount(() => {
          dialog.showModal();
          measure();
          window.addEventListener("resize", measure);
          onCleanup(() => window.removeEventListener("resize", measure));
        });
        return (
          <dialog
            ref={dialog}
            class="image-viewer"
            aria-label={shown().alt || t("Image")}
            onCancel={(event) => {
              event.preventDefault();
              close();
            }}
            onPointerDown={(event) => (outside = event.button === 0 && event.target !== image)}
            onClick={(event) => {
              const dismiss = outside && event.target !== image;
              outside = false;
              if (dismiss) close();
            }}
            onKeyDown={(event) => {
              if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
              if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
              // The next image is the one after it in reading order, to the left in Arabic.
              const forward = (event.key === "ArrowRight") !== (document.documentElement.dir === "rtl");
              const next = index() + (forward ? 1 : -1);
              event.preventDefault();
              if (next >= 0 && next < current.images.length) setIndex(next);
            }}
          >
            <img ref={image} class="image-viewer-image" src={shown().src} alt={shown().alt} style={size()} />
            <Button class="image-viewer-close" icon="close" label={t("Close")} onClick={close} />
          </dialog>
        );
      }}
    </Show>
  );
}
