import { markdownBlockIndex } from "./markdown-dom";
import { selectionLines, sourceBlocks, type Boundary, type Segment, type SourceNode, type SourceRange } from "./markdown-source";

/**
 * Where one end of a DOM selection sits in a rendered Markdown host: its
 * block, the list items, quoted children and table rows around it, and its
 * line inside a code block. An end on the host itself stands for the child at
 * its offset.
 */
export function boundaryOf(host: HTMLElement, container: Node, offset: number): Boundary | undefined {
  let node: Node = container;
  if (container === host) {
    const child = host.childNodes[Math.min(offset, host.childNodes.length - 1)];
    if (!child) return undefined;
    node = child;
  }
  const block = markdownBlockIndex(host, node);
  if (block === undefined) return undefined;
  const path: Segment[] = [];
  let codeLine: number | undefined;
  for (let current: Node | null = node; current && current !== host; current = current.parentNode) {
    if (!(current instanceof Element)) continue;
    const parent = current.parentElement;
    if (current.tagName === "CODE" && parent?.tagName === "PRE" && codeLine === undefined) {
      // The line of the code the end sits on: the line breaks before it.
      const range = document.createRange();
      range.setStart(current, 0);
      try {
        range.setEnd(container, offset);
        codeLine = (range.toString().match(/\n/g) ?? []).length;
      } catch {
        codeLine = undefined;
      }
    }
    if (current.tagName === "LI" && parent && /^(UL|OL)$/.test(parent.tagName))
      path.unshift({ kind: "item", index: Array.from(parent.children).filter((item) => item.tagName === "LI").indexOf(current) });
    else if (current.tagName === "TR") {
      const table = current.closest("table");
      if (table) path.unshift({ kind: "row", index: Array.from(table.rows).indexOf(current as HTMLTableRowElement) });
    } else if (parent?.tagName === "BLOCKQUOTE")
      path.unshift({ kind: "child", index: Array.from(parent.children).indexOf(current) });
  }
  return { block, path, ...(codeLine !== undefined && { codeLine }) };
}

/** The source lines a selection in a Preview covers, or none when it is not one. */
export function previewLines(
  host: HTMLElement,
  source: string,
  selection: Selection | null,
  blocks: SourceNode[] = sourceBlocks(source),
): SourceRange | undefined {
  if (!selection || selection.isCollapsed || !selection.rangeCount) return undefined;
  const range = selection.getRangeAt(0);
  if (!host.contains(range.startContainer) || !host.contains(range.endContainer)) return undefined;
  const start = boundaryOf(host, range.startContainer, range.startOffset);
  let end = boundaryOf(host, range.endContainer, range.endOffset);
  // A selection that ends at the very start of a block (as a triple click
  // does) ends with the text before it.
  if (start && end && range.endOffset === 0 && end.block > start.block) {
    const before = textBefore(host, range.endContainer);
    if (before) end = boundaryOf(host, before, before.length) ?? end;
  }
  if (!start || !end) return undefined;
  return selectionLines(source, blocks, start, end, selection.toString());
}

/** The last text in `host` that comes before `node`. */
function textBefore(host: HTMLElement, node: Node): Text | undefined {
  const walker = document.createTreeWalker(host, NodeFilter.SHOW_TEXT);
  let last: Text | undefined;
  for (let text = walker.nextNode(); text; text = walker.nextNode()) {
    if (text === node || node.contains(text) || !(node.compareDocumentPosition(text) & Node.DOCUMENT_POSITION_PRECEDING)) break;
    if ((text as Text).data.trim()) last = text as Text;
  }
  return last;
}
