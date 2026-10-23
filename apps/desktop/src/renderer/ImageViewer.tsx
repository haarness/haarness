import { t } from "./i18n";
import { Show, createSignal, onMount } from "solid-js";
import { Button } from "./ui";

/*
 * A conversation's images open full size here: an attachment, an image in a
 * reply, thinking or a side chat, and a read tool's preview. It shows the
 * address the image already shows (a data or blob URL), so opening it loads
 * nothing new. Esc, a click outside the image or Close shuts it, and the
 * keyboard is back on the image that opened it. The arrow keys step through
 * the other images of the same conversation on screen.
 */

/** The images the viewer opens: ones the page already holds, never an address it would fetch. */
const holds = (src: string | null | undefined) => !!src && /^(?:data:image\/|blob:)/i.test(src.trim());

const picture = (element: Element) =>
  element instanceof HTMLImageElement ? element : element.querySelector("img");

/**
 * Marks each image under `root` that opens larger: focusable, and a button
 * to assistive technology. An image inside a link keeps the link's click.
 * Markdown marks every render the same way, so its patches keep the marks.
 */
export function markOpenImages(root: ParentNode, unnamed: string) {
  for (const image of Array.from(root.querySelectorAll("img"))) {
    if (!holds(image.getAttribute("src")) || image.closest("a")) continue;
    image.setAttribute("data-image-open", "");
    image.setAttribute("tabindex", "0");
    image.setAttribute("role", "button");
    image.setAttribute("aria-haspopup", "dialog");
    if (!image.getAttribute("alt")) image.setAttribute("aria-label", unnamed);
  }
}

type Viewing = { images: { src: string; alt: string }[]; index: number; opener: HTMLElement };
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
      return { src: image.getAttribute("src")!, alt: image.getAttribute("alt") || "" };
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
        const close = () => {
          if (viewing() !== current) return;
          dialog.close();
          setViewing(undefined);
          // Shown from code, the dialog has no trigger to hand the keyboard back to.
          if (current.opener.isConnected) current.opener.focus({ preventScroll: true });
        };
        // Only a whole click outside the image closes it, never a drag that ends there.
        let outside = false;
        onMount(() => dialog.showModal());
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
            <img ref={image} class="image-viewer-image" src={shown().src} alt={shown().alt} />
            <Button class="image-viewer-close" icon="close" label={t("Close")} onClick={close} />
          </dialog>
        );
      }}
    </Show>
  );
}
