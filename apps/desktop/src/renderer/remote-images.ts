/*
 * Images a reply links to on another host. The page's CSP loads none of
 * them (it allows only Haarness Cloud, for marketplace icons, and Markdown
 * never puts a remote address in an `<img>`), so `![x](https://host/?d=secret)`
 * tells that host nothing: Markdown shows a placeholder naming the host with
 * a "Load image from <host>" button, and only that click fetches the image,
 * in the main process (main/remote-image.ts), as a data URL. Loaded images
 * are kept for their chat in memory, so a reply drawn again shows them at
 * once; a restart forgets them.
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

const pending = new Map<string, Promise<string>>();
type Settled = (scope: string, src: string, data: string | undefined) => void;
const listeners = new Set<Settled>();

/**
 * Calls `listener` whenever a fetch settles, with the image or undefined when
 * it failed. Every mounted reply listens, so each swaps in, or turns back on,
 * its own placeholders: a reply drawn anew while the fetch ran (scrolled back
 * into view, or another reply with the same image) is not left waiting.
 */
export function onImageSettled(listener: Settled): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function settle(scope: string, src: string, data: string | undefined) {
  for (const listener of [...listeners]) {
    try {
      listener(scope, src, data);
    } catch {
      /* One reply's trouble never keeps the others waiting. */
    }
  }
}

/**
 * Loads an image for a chat once at a time: a second click, or another reply
 * showing it, waits for the fetch already running. The loaded image is
 * remembered for the chat, and the fetch is over (imageLoading is false)
 * before listeners hear of it.
 */
export function loadImage(scope: string, src: string, fetch: (src: string) => Promise<string>): Promise<string> {
  const entry = key(scope, src);
  let running = pending.get(entry);
  if (!running) {
    running = fetch(src).then(
      (data) => {
        rememberImage(scope, src, data);
        pending.delete(entry);
        settle(scope, src, data);
        return data;
      },
      (error: unknown) => {
        pending.delete(entry);
        settle(scope, src, undefined);
        throw error;
      },
    );
    pending.set(entry, running);
  }
  return running;
}

/** Whether this chat is fetching the image now. */
export function imageLoading(scope: string, src: string) {
  return pending.has(key(scope, src));
}

/** Forgets every loaded image, as quitting does. */
export function forgetImages() {
  remembered.clear();
  rememberedBytes = 0;
}

/**
 * Placeholders and their buttons made here, and the project's images and
 * their placeholders (project-images.ts). A reply's own markup is never one,
 * whatever classes or attributes it copies, so only these load images.
 */
const made = new WeakSet<Node>();
export function madeHere(node: Node | null | undefined): boolean {
  return !!node && made.has(node);
}
/** Marks a node the app made in a reply, which the reply's own markup can never be. */
export function markMade(node: Node) {
  made.add(node);
}

/**
 * What the sanitizer keeps of an attribute a reply wrote on `element` (its
 * tag name): never the `data-remote-*` and `data-project-*` attributes
 * placeholders use, nor the mark of an image that opens larger, so markup
 * cannot dress as one. Classes are the app's own: one a reply borrowed would
 * be styled as the app styles it, so `<div class="modal-backdrop">` would lay
 * the app's overlay over the chat and `remote-image` would pass for a
 * placeholder. So a reply keeps no class but the `language-…` that Markdown
 * writes on a code block's `code`, which names its language. Undefined drops
 * the attribute.
 */
export function replyAttribute(name: string, value: string, element: string): string | undefined {
  if (/^data-(?:remote|project)-|^data-image-open$/i.test(name)) return undefined;
  if (name.toLowerCase() !== "class") return value;
  if (element.toLowerCase() !== "code") return undefined;
  const kept = value.split(/\s+/).filter((token) => /^language-./i.test(token));
  return kept.length ? kept.join(" ") : undefined;
}

/** What every id a reply's markup writes starts with (`replyIds`). */
export const REPLY_ID = "reply-";
/** Attributes that name ids of the same page, one or several. */
const ID_REFERENCES = new Set([
  "aria-activedescendant", "aria-controls", "aria-describedby", "aria-details", "aria-errormessage",
  "aria-flowto", "aria-labelledby", "aria-owns", "for", "headers", "itemref",
]);

/**
 * A reply's ids are its own: the app finds parts of its own by id (the
 * message box's command menu scrolls to `slash-command-1`, and names it to
 * assistive technology), and a reply drawn before them in the page would be
 * found instead. So each id a reply writes gets `REPLY_ID` in front, and so
 * does each reference to one inside the reply (an SVG's `url(#shade)` or
 * `href="#shape"`, a label's `for`, an `aria-labelledby`), so a drawing
 * still finds its gradient. `svg` says the attribute is on an SVG element,
 * where `href="#…"` names one of the reply's elements.
 */
export function replyIds(name: string, value: string, svg: boolean): string {
  const lower = name.toLowerCase();
  if (lower === "id") return value ? REPLY_ID + value : value;
  if (ID_REFERENCES.has(lower))
    return value.split(/\s+/).filter(Boolean).map((id) => REPLY_ID + id).join(" ");
  if (svg && (lower === "href" || lower === "xlink:href") && value.trim().startsWith("#"))
    return "#" + REPLY_ID + value.trim().slice(1);
  return value.replace(/url\((\s*['"]?\s*)#/gi, (_match, before: string) => `url(${before}#${REPLY_ID}`);
}

/**
 * The addresses one click on `host`'s button loads: each placeholder of that
 * host whose address really is on that host, so the host the button names is
 * the only one asked.
 */
export function loadableSources(placeholders: { host?: string; src?: string }[], host: string): string[] {
  return [
    ...new Set(
      placeholders
        .filter((placeholder) => placeholder.host === host && remoteImageHost(placeholder.src) === host)
        .map((placeholder) => placeholder.src!),
    ),
  ];
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
export function holdRemoteImages(
  root: ParentNode,
  loaded: (src: string) => string | undefined,
  labels: RemoteImageLabels,
  loading: (src: string) => boolean = () => false,
) {
  for (const image of Array.from(root.querySelectorAll("img"))) {
    const src = image.getAttribute("src")?.trim();
    const host = remoteImageHost(src);
    if (!src || !host) continue;
    const alt = image.getAttribute("alt") ?? "";
    const data = loaded(src);
    image.replaceWith(
      data ? loadedElement(image.ownerDocument, src, alt, data) : placeholder(image.ownerDocument, src, host, alt, labels, loading(src)),
    );
  }
}

/**
 * Words the placeholders under `root` in the interface language again: a
 * reply already drawn is not drawn anew when the language changes.
 */
export function relabelImages(root: ParentNode, labels: RemoteImageLabels) {
  for (const holder of Array.from(root.querySelectorAll<HTMLElement>(".remote-image"))) {
    if (!madeHere(holder)) continue;
    const text = holder.querySelector(".remote-image-alt");
    const button = holder.querySelector(".remote-image-load");
    if (text) text.textContent = holder.dataset.remoteAlt || labels.unnamed;
    if (button && madeHere(button)) button.textContent = labels.load(holder.dataset.remoteHost ?? "");
  }
}

function placeholder(document: Document, src: string, host: string, alt: string, labels: RemoteImageLabels, busy: boolean) {
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
  // A fetch already running keeps its button off, however often the reply is drawn.
  button.disabled = busy;
  holder.append(text, button);
  made.add(holder);
  made.add(button);
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
