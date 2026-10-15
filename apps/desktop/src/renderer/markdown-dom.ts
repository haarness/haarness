import DOMPurify from "dompurify";
import type { MarkdownPatch } from "./markdown-project";

type Block = { html: string; start: Comment; end: Comment };

const URL_ATTRIBUTES = new Set(["href", "xlink:href", "src", "srcset", "poster", "background"]);
// Every link click goes through Markdown.tsx, which decides where it may go
// and says why a link does not open. So a link keeps a `file:` address, a
// file share's address or a Windows drive path, none of which can run
// script, instead of losing it to the default rules; any other scheme those
// rules would drop (`javascript:`, `vscode:`) becomes a placeholder that only
// says it does not open. No other address may name a network path: an image
// there would reach its host as soon as the reply is drawn, on Windows with
// the user's sign-in.
DOMPurify.addHook("uponSanitizeAttribute", (node, data) => {
  if (!URL_ATTRIBUTES.has(data.attrName)) return;
  if (node.nodeName === "A" && data.attrName === "href") {
    const value = data.attrValue;
    if (/^(?:(?:file|smb|afp|nfs|cifs|dav|davs|webdav|webdavs):|[a-z]:[\\/])/i.test(value))
      data.forceKeepAttr = true;
    else if (/^[a-z][a-z\d+.-]*:/i.test(value) && !/^(?:https?|mailto):/i.test(value)) {
      // A kept attribute is not rewritten from `attrValue`, so replace it here.
      (node as Element).setAttribute("href", "unsupported:");
      data.forceKeepAttr = true;
    }
    return;
  }
  if (data.attrValue.split(",").some((part) => /^[\\/]{2}/.test(part.trim())))
    data.keepAttr = false;
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
    if (current.nodeType !== target.nodeType || current.nodeName !== target.nodeName ||
      (current instanceof Element && target instanceof Element && current.namespaceURI !== target.namespaceURI)) {
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

export function createMarkdownDOM(host: HTMLElement) {
  const blocks: Block[] = [];
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
      const fragment = DOMPurify.sanitize(html, {
        RETURN_DOM_FRAGMENT: true,
        FORBID_TAGS: ["style", "form", "input", "button", "iframe"],
        FORBID_ATTR: ["style"],
      });
      nodes(host, between(block), Array.from(fragment.childNodes), block.end);
      block.html = html;
    });
    for (const block of blocks.splice(patch.count)) {
      for (const node of [block.start, ...between(block), block.end]) host.removeChild(node);
    }
  };
}
