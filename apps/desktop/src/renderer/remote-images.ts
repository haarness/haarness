/*
 * Images a reply links to on another host. The page's CSP loads none of
 * them, so `![x](https://host/?d=secret)` tells that host nothing: Markdown
 * shows a placeholder naming the host with a "Load image from <host>"
 * button, and only that click fetches the image, in the main process
 * (main/remote-image.ts), as a data URL. Loaded images are kept for their
 * chat in memory, so a reply drawn again shows them at once; a restart
 * forgets them.
 */

/** The host an image source would be fetched from, or undefined for one that stays in the page. */
export function remoteImageHost(src: string | null | undefined): string | undefined {
  if (!src) return undefined;
  try {
    const url = new URL(src.trim());
    return (url.protocol === "http:" || url.protocol === "https:") && url.hostname ? url.hostname : undefined;
  } catch {
    // A relative path names this page's own files, never another host.
    return undefined;
  }
}

/** At most this much image data is kept across all chats; the oldest go first. */
export const REMEMBERED_BYTES = 64 * 1024 * 1024;
const remembered = new Map<string, string>();
let rememberedBytes = 0;
const key = (scope: string, src: string) => `${scope}\n${src}`;

/** An image this chat already loaded, as a data URL. */
export function loadedImage(scope: string, src: string): string | undefined {
  return remembered.get(key(scope, src));
}

/** Keeps a loaded image for its chat until the app quits or newer images need the room. */
export function rememberImage(scope: string, src: string, data: string, limit = REMEMBERED_BYTES) {
  const entry = key(scope, src);
  const previous = remembered.get(entry);
  if (previous !== undefined) {
    rememberedBytes -= previous.length;
    remembered.delete(entry);
  }
  if (data.length > limit) return;
  remembered.set(entry, data);
  rememberedBytes += data.length;
  for (const [oldest, value] of remembered) {
    if (rememberedBytes <= limit) break;
    remembered.delete(oldest);
    rememberedBytes -= value.length;
  }
}

/** Forgets every loaded image, as quitting does. */
export function forgetImages() {
  remembered.clear();
  rememberedBytes = 0;
}

export interface RemoteImageLabels {
  /** The button: "Load image from {host}". */
  load: (host: string) => string;
  /** Shown for an image with no alt text. */
  unnamed: string;
}

/**
 * Replaces each image from another host under `root` with the image this
 * chat already loaded or a placeholder: its alt text and a button naming the
 * host. The button carries the host as its own text, so alt text that reads
 * like a host cannot pass for it.
 */
export function holdRemoteImages(root: ParentNode, loaded: (src: string) => string | undefined, labels: RemoteImageLabels) {
  for (const image of Array.from(root.querySelectorAll("img"))) {
    const src = image.getAttribute("src")?.trim();
    const host = remoteImageHost(src);
    if (!src || !host) continue;
    const alt = image.getAttribute("alt") ?? "";
    const data = loaded(src);
    image.replaceWith(data ? loadedElement(image.ownerDocument, src, alt, data) : placeholder(image.ownerDocument, src, host, alt, labels));
  }
}

function placeholder(document: Document, src: string, host: string, alt: string, labels: RemoteImageLabels) {
  const holder = document.createElement("span");
  holder.className = "remote-image";
  holder.dataset.remoteSrc = src;
  holder.dataset.remoteHost = host;
  holder.dataset.remoteAlt = alt;
  const text = document.createElement("span");
  text.className = "remote-image-alt";
  text.textContent = alt || labels.unnamed;
  const button = document.createElement("button");
  button.type = "button";
  button.className = "remote-image-load";
  button.title = src;
  button.textContent = labels.load(host);
  holder.append(text, button);
  return holder;
}

/** The loaded image, in the place its placeholder had. */
export function loadedElement(document: Document, src: string, alt: string, data: string) {
  const image = document.createElement("img");
  image.className = "remote-image-loaded";
  image.dataset.remoteSrc = src;
  image.alt = alt;
  image.src = data;
  return image;
}
