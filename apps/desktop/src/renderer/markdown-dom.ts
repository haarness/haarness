import DOMPurify from "dompurify";
import { cssReferencesOutside, loadsNetworkPath, parsedAddress } from "./link-target";
import type { MarkdownPatch } from "./markdown-project";
import { holdRemoteImages, madeHere, replyAttribute, type RemoteImageLabels } from "./remote-images";
import { holdProjectImages, type ProjectImageLabels, type ShownImage } from "./project-images";
import { markOpenImages } from "./ImageViewer";

type Block = { html: string; start: Comment; end: Comment };

const URL_ATTRIBUTES = new Set(["href", "xlink:href", "src", "srcset", "poster", "background"]);
// SVG presentation attributes read as CSS, whose `url()` names what to draw.
const CSS_URL_ATTRIBUTES = new Set([
  "fill", "stroke", "filter", "mask", "clip-path", "marker", "marker-start", "marker-mid", "marker-end", "cursor",
]);
// Markdown has a sanitizer of its own: a hook or setting another part of the
// app (or a library it loads, as Mermaid does) adds to the shared DOMPurify
// never changes what a reply may contain, and the hook below, which keeps
// `file:` and file-share links, stays out of theirs.
const purifier = DOMPurify(window);
/**
 * Each image's source as the reply wrote it, before the rules below or the
 * default ones decide on it: a local `file:` URL, which they drop, still
 * names a file of the project (project-images.ts). Only a reply drawn in the
 * conversation reads it, and only through the server.
 */
const written = new WeakMap<Element, string>();
// Every link click goes through Markdown.tsx, which decides where it may go
// and says why a link does not open. So a link keeps a `file:` address, a
// file share's address or a Windows drive path, none of which can run
// script, instead of losing it to the default rules; any other scheme those
// rules would drop (`javascript:`, `vscode:`) becomes a placeholder that only
// says it does not open. No other address may name a network path: an image
// there would reach its host as soon as the reply is drawn, on Windows with
// the user's sign-in. Each is read as the URL parser reads it, which drops
// tabs and newlines anywhere, so `/<TAB>/host/x.png` names one too. A CSS
// `url()` (an SVG `mask`, `filter`, `fill`…) may point only into the reply.
purifier.addHook("uponSanitizeAttribute", (node, data) => {
  // Image placeholders are the app's own (remote-images.ts): a reply keeps
  // none of their classes or data attributes, so it cannot forge one.
  const kept = replyAttribute(data.attrName, data.attrValue);
  if (kept === undefined) {
    data.keepAttr = false;
    return;
  }
  data.attrValue = kept;
  if (node.nodeName === "IMG" && data.attrName === "src") written.set(node as Element, kept);
  if (!URL_ATTRIBUTES.has(data.attrName)) {
    const css = CSS_URL_ATTRIBUTES.has(data.attrName) || /url\(/i.test(data.attrValue);
    if (css && cssReferencesOutside(data.attrValue)) data.keepAttr = false;
    return;
  }
  if (node.nodeName === "A" && data.attrName === "href") {
    const value = parsedAddress(data.attrValue);
    if (/^(?:(?:file|smb|afp|nfs|cifs|dav|davs|webdav|webdavs):|[a-z]:[\\/])/i.test(value))
      data.forceKeepAttr = true;
    else if (/^[a-z][a-z\d+.-]*:/i.test(value) && !/^(?:https?|mailto):/i.test(value)) {
      // A kept attribute is not rewritten from `attrValue`, so replace it here.
      (node as Element).setAttribute("href", "unsupported:");
      data.forceKeepAttr = true;
    }
    return;
  }
  if (loadsNetworkPath(data.attrValue)) data.keepAttr = false;
});

// Patch only the changed interval. Assigning textContent/innerHTML resets
// browser Range endpoints, including a selection in an unchanged prefix.
function text(current: Text, next: string) {
  const previous = current.data;
  if (previous === next) return;
  let prefix = 0;
  while (prefix < previous.length && prefix < next.length && previous[prefix] === next[prefix]) prefix++;
  let suffix = 0;
  while (suffix < previous.length - prefix && suffix < next.length - prefix && previous.at(-suffix - 1) === next.at(-suffix - 1)) suffix++;
  current.replaceData(prefix, previous.length - prefix - suffix, next.slice(prefix, next.length - suffix));
}

function nodes(parent: Node, previous: Node[], next: Node[], end: Node | null = null) {
  for (let index = 0; index < next.length; index++) {
    const target = next[index];
    const current = previous[index];
    if (!current) { parent.insertBefore(target, end); continue; }
    // An image placeholder is replaced, never patched from or into a reply's
    // own element, so only placeholders the app made carry its button.
    if (current.nodeType !== target.nodeType || current.nodeName !== target.nodeName ||
      (current instanceof Element && target instanceof Element && current.namespaceURI !== target.namespaceURI) ||
      madeHere(current) !== madeHere(target)) {
      parent.replaceChild(target, current);
      continue;
    }
    if (current instanceof Text) { text(current, target.textContent ?? ""); continue; }
    if (!(current instanceof Element) || !(target instanceof Element)) continue;
    for (const attribute of current.getAttributeNames()) {
      if (attribute === "data-language") continue;
      if (!target.hasAttribute(attribute)) current.removeAttribute(attribute);
    }
    for (const attribute of target.attributes) {
      if (current.getAttribute(attribute.name) !== attribute.value) current.setAttribute(attribute.name, attribute.value);
    }
    const copy = current.querySelector(":scope > .code-copy");
    nodes(current, Array.from(current.childNodes).filter((node) => node !== copy), Array.from(target.childNodes), copy);
  }
  for (const current of previous.slice(next.length)) parent.removeChild(current);
}

/** Each Markdown host's blocks, in order, for finding which block a node is in. */
const hosts = new WeakMap<HTMLElement, Block[]>();

/**
 * The index of the block `node` belongs to in a Markdown host: the block whose
 * markers enclose the host's child that holds it. Blocks are drawn one per
 * top-level token, so this is that token's index (see markdown-source.ts).
 */
export function markdownBlockIndex(host: HTMLElement, node: Node): number | undefined {
  const blocks = hosts.get(host);
  if (!blocks || !host.contains(node) || node === host) return undefined;
  let top = node;
  while (top.parentNode && top.parentNode !== host) top = top.parentNode;
  for (let current: Node | null = top; current; current = current.previousSibling) {
    if (!(current instanceof Comment)) continue;
    const index = blocks.findIndex((block) => block.start === current || block.end === current);
    if (index >= 0) return blocks[index].end === current && current !== top ? index + 1 : index;
  }
  return undefined;
}

/**
 * `images` decides what an image from another host shows: the one this chat
 * already loaded, or a placeholder whose button loads it (remote-images.ts).
 * With `project`, as in the conversation, an image whose source is a path
 * shows the file of the project it names, or a placeholder while it is read
 * (project-images.ts), and every image the page holds opens larger.
 */
export function createMarkdownDOM(
  host: HTMLElement,
  images?: {
    loaded: (src: string) => string | undefined;
    loading: (src: string) => boolean;
    labels: () => RemoteImageLabels;
    project?: {
      /** The project's folder, absolute, as the server reports it. */
      root: () => string;
      shown: (path: string) => ShownImage | undefined;
      labels: () => ProjectImageLabels;
    };
  },
) {
  const blocks: Block[] = [];
  hosts.set(host, blocks);
  const between = (block: Block) => {
    const result: Node[] = [];
    for (let node = block.start.nextSibling; node && node !== block.end; node = node.nextSibling) result.push(node);
    return result;
  };
  return (patch: MarkdownPatch) => {
    patch.html.forEach((html, offset) => {
      const index = patch.start + offset;
      let block = blocks[index];
      if (!block) {
        block = { html: "", start: document.createComment(""), end: document.createComment("") };
        blocks.push(block);
        host.append(block.start, block.end);
      }
      if (block.html === html) return;
      const fragment = purifier.sanitize(html, {
        RETURN_DOM_FRAGMENT: true,
        FORBID_TAGS: ["style", "form", "input", "button", "iframe"],
        FORBID_ATTR: ["style"],
      });
      const project = images?.project;
      if (project)
        holdProjectImages(
          fragment,
          project.root(),
          (image) => written.get(image) ?? image.getAttribute("src"),
          project.shown,
          project.labels(),
        );
      if (images) holdRemoteImages(fragment, images.loaded, images.labels(), images.loading);
      if (project) markOpenImages(fragment, project.labels().unnamed);
      nodes(host, between(block), Array.from(fragment.childNodes), block.end);
      block.html = html;
    });
    for (const block of blocks.splice(patch.count)) {
      for (const node of [block.start, ...between(block), block.end]) host.removeChild(node);
    }
  };
}
