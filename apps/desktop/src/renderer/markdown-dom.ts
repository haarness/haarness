import DOMPurify from "dompurify";
import { cssReferencesOutside, loadsNetworkPath, parsedAddress } from "./link-target";
import type { MarkdownPatch } from "./markdown-project";
import { holdRemoteImages, madeHere, markMade, replyAttribute, replyIds, type RemoteImageLabels } from "./remote-images";
import { holdProjectImages, type ProjectImageLabels, type ShownImage } from "./project-images";
import { openMark } from "./ImageViewer";
import { codeBlockShows, hasCodeCharacters, spelledCharacter, spelledOutParts } from "./display-text";

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
  // Classes and image placeholders are the app's own (remote-images.ts): a
  // reply keeps no class but a code block's language, and none of the
  // placeholders' data attributes, so it can neither forge a placeholder nor
  // wear the app's styles.
  const kept = replyAttribute(data.attrName, data.attrValue, node.nodeName);
  if (kept === undefined) {
    data.keepAttr = false;
    return;
  }
  data.attrValue = kept;
  if (node.nodeName === "IMG" && data.attrName === "src") written.set(node as Element, kept);
  // Its ids are its own, and so are its references to them.
  data.attrValue = replyIds(data.attrName, data.attrValue, node.namespaceURI === "http://www.w3.org/2000/svg");
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
      // The language label and an image's marks for opening it are the app's, set after drawing.
      if (attribute === "data-language" || openMark(current, attribute)) continue;
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
 * (project-images.ts).
 */
/**
 * A task list's boxes (`- [x] done`), the one input a reply keeps: each is
 * replaced by a new checkbox that only says whether it is ticked and cannot
 * be changed. Every other input the reply wrote is dropped.
 */
function taskBoxes(fragment: DocumentFragment) {
  for (const input of Array.from(fragment.querySelectorAll("input"))) {
    const item = input.parentElement?.closest("li");
    if (input.getAttribute("type")?.toLowerCase() !== "checkbox" || !item) {
      input.remove();
      continue;
    }
    const box = document.createElement("input");
    box.type = "checkbox";
    box.disabled = true;
    box.checked = input.hasAttribute("checked");
    if (box.checked) box.setAttribute("checked", "");
    box.className = "task-box";
    item.classList.add("task-item");
    input.replaceWith(box);
  }
}

/**
 * Inside a code block, raw HTML that hides part of it without CSS (`hidden`,
 * a closed `<details>`, a `<template>`) or turns its direction (`dir`) is
 * taken away, so the block shows all it holds, in the order it holds it: a
 * `<details>` or `<summary>` gives way to what it holds, and a `<template>`,
 * which never shows, goes.
 */
function plainBlocks(fragment: DocumentFragment) {
  for (const block of Array.from(fragment.querySelectorAll("pre"))) {
    for (const element of [block, ...Array.from(block.querySelectorAll("*"))]) {
      element.removeAttribute("hidden");
      element.removeAttribute("dir");
    }
    for (const element of Array.from(block.querySelectorAll("template"))) element.remove();
    for (const element of Array.from(block.querySelectorAll("details, summary")))
      element.replaceWith(...Array.from(element.childNodes));
  }
}

/**
 * A code block's characters that would not show, or would draw its code in
 * another order than the one it is copied in (an escape, a direction
 * override), each shown as its code (`\u{1B}`) on a quiet chip, as an
 * approval card shows them (display-text.ts). The chips are the app's own,
 * and Copy turns each back into its character (`shownCode`).
 */
function spellCode(fragment: DocumentFragment) {
  for (const block of Array.from(fragment.querySelectorAll("pre"))) {
    // Most code holds none: it is not walked character by character.
    if (!hasCodeCharacters(block.textContent ?? "")) continue;
    const walker = document.createTreeWalker(block, NodeFilter.SHOW_TEXT);
    const texts: Text[] = [];
    for (let node = walker.nextNode(); node; node = walker.nextNode()) texts.push(node as Text);
    for (const node of texts) {
      const parts = spelledOutParts(node.data, codeBlockShows);
      if (!parts.some((part) => part.code)) continue;
      node.replaceWith(
        ...parts.map((part) => {
          if (!part.code) return part.text;
          const chip = document.createElement("span");
          chip.className = "spelled-code";
          chip.textContent = part.text;
          markMade(chip);
          return chip;
        }),
      );
    }
  }
}

/**
 * The code a block's Copy copies, when the block shows it alone: one `code`
 * holding only text and the app's chips, and nothing else in the block but
 * whitespace and its Copy button. Markdown's code blocks are all so. Raw HTML
 * with more in its `pre` (text beside the code, several codes, markup inside
 * one) gets no Copy, since what Copy put on the clipboard could differ from
 * what the block shows.
 */
export function copiedCode(block: Element): HTMLElement | undefined {
  let code: HTMLElement | undefined;
  for (const node of Array.from(block.childNodes)) {
    if (node instanceof Comment || (node instanceof Text && !node.data.trim())) continue;
    if (node instanceof HTMLButtonElement && node.classList.contains("code-copy")) continue;
    if (!code && node instanceof HTMLElement && node.localName === "code") code = node;
    else return undefined;
  }
  for (const node of Array.from(code?.childNodes ?? [])) {
    if (node instanceof Comment || node instanceof Text) continue;
    if (!(node instanceof HTMLElement && madeHere(node) && node.matches(".spelled-code"))) return undefined;
  }
  return code;
}

/** What a code block shows of `code`, as Copy copies it: its text, with
 * each of the app's chips (`\u{1B}`) the character it stands for again. */
export function shownCode(code: Element) {
  let text = "";
  for (const node of Array.from(code.childNodes)) {
    if (node instanceof Text) text += node.data;
    else if (node instanceof HTMLElement && madeHere(node)) text += spelledCharacter(node.textContent ?? "") ?? "";
  }
  return text;
}

export function createMarkdownDOM(
  host: HTMLElement,
  images?: {
    loaded: (src: string) => string | undefined;
    loading: (src: string) => boolean;
    labels: () => RemoteImageLabels;
    project?: {
      /** The project's folders, absolute (`projectRoots`). */
      root: () => readonly string[];
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
      // The app finds its own parts by tag, attribute and role (an open
      // dialog, its message box, a data attribute, a tab stop), so a reply
      // writes none of them: such an element gives way to its text, and
      // such an attribute goes. The app writes its own data attributes after
      // this, and task boxes replace every input (`taskBoxes`).
      const fragment = purifier.sanitize(html, {
        RETURN_DOM_FRAGMENT: true,
        FORBID_TAGS: ["style", "form", "button", "iframe", "dialog", "textarea", "select"],
        FORBID_ATTR: [
          "style", "role", "aria-modal", "popover", "popovertarget", "popovertargetaction", "inert", "autofocus", "tabindex",
        ],
        ALLOW_DATA_ATTR: false,
      });
      taskBoxes(fragment);
      plainBlocks(fragment);
      spellCode(fragment);
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
      nodes(host, between(block), Array.from(fragment.childNodes), block.end);
      block.html = html;
    });
    for (const block of blocks.splice(patch.count)) {
      for (const node of [block.start, ...between(block), block.end]) host.removeChild(node);
    }
  };
}
