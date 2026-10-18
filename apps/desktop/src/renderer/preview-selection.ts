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
    if (current.tagName === "LI" && parent && /^(UL|OL)$/.test(parent.tagName)) {
      // Items nested in an item count across all of its lists, as the source
      // map lists them: an item may hold a list, a paragraph, then another list.
      const outer = parent.parentElement?.tagName === "LI" ? parent.parentElement : undefined;
      const siblings = outer
        ? Array.from(outer.querySelectorAll(":scope > ul > li, :scope > ol > li"))
        : Array.from(parent.children).filter((item) => item.tagName === "LI");
      path.unshift({ kind: "item", index: siblings.indexOf(current) });
    }
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
  // A selection that ends at the start of a line (Shift+Down, a triple
  // click, a drag to the next item) does not cover that line: it ends with
  // the last character it does cover, in one block or across several.
  const text = selection.toString();
  if (start && end && (/\n\s*$/.test(text) || range.endOffset === 0)) {
    const last = lastSelected(host, range);
    if (last) end = boundaryOf(host, last.node, last.offset) ?? end;
  }
  if (!start || !end) return undefined;
  return selectionLines(source, blocks, start, end, selection.toString());
}

/** Just after the last non-blank character the range covers, or none when it covers none. */
function lastSelected(host: HTMLElement, range: Range): { node: Text; offset: number } | undefined {
  const walker = document.createTreeWalker(host, NodeFilter.SHOW_TEXT);
  const texts: Text[] = [];
  for (let node = walker.nextNode(); node; node = walker.nextNode())
    if (range.intersectsNode(node) && !(node.parentElement?.closest(".code-copy"))) texts.push(node as Text);
  for (let index = texts.length - 1; index >= 0; index--) {
    const node = texts[index];
    const from = node === range.startContainer ? range.startOffset : 0;
    const to = node === range.endContainer ? range.endOffset : node.data.length;
    const covered = node.data.slice(from, to);
    const at = covered.search(/\S\s*$/);
    if (at >= 0) return { node, offset: from + at + 1 };
  }
  return undefined;
}
