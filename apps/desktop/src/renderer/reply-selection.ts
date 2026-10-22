import { isTextEntry } from "./text-entry";

/**
 * A selection in a reply, as text offsets in the reply's own text: where it
 * starts (`anchor`) and where the person dragged to (`focus`), so a backward
 * selection stays backward, and the text between them.
 */
export interface ReplySelection {
  anchor: number;
  focus: number;
  text: string;
}

/** Where `offset` falls in text split into pieces of these `lengths`: the
 * piece and the offset in it. At a seam between two pieces, a selection's
 * `start` goes to the beginning of the later piece and its `end` to the end
 * of the earlier one, as the browser draws them. */
export function textPoint(
  lengths: readonly number[],
  offset: number,
  edge: "start" | "end",
): [number, number] | undefined {
  if (offset < 0) return undefined;
  let before = 0;
  for (let index = 0; index < lengths.length; index++) {
    const after = before + lengths[index];
    if (edge === "start" ? offset < after : offset <= after)
      return [index, offset - before];
    before = after;
  }
  return undefined;
}

/** Where a selection saved from one reply goes in a reply drawn from the
 * same text (in these pieces): only when that text holds the same words at
 * the same offsets, so the wrong text is never selected. */
export function restorePlan(saved: ReplySelection, pieces: readonly string[]) {
  const start = Math.min(saved.anchor, saved.focus);
  const end = Math.max(saved.anchor, saved.focus);
  if (start === end || pieces.join("").slice(start, end) !== saved.text)
    return undefined;
  const lengths = pieces.map((piece) => piece.length);
  const first = textPoint(lengths, start, "start");
  const last = textPoint(lengths, end, "end");
  if (!first || !last) return undefined;
  return saved.anchor <= saved.focus
    ? { anchor: first, focus: last }
    : { anchor: last, focus: first };
}

/** The reply's text, piece by piece in reading order, without the app's own
 * controls drawn into it (a code block's Copy button, an image's Load
 * button), which only one of the two replies has. */
function textNodes(root: Node) {
  const nodes: Text[] = [];
  const walker = root.ownerDocument!.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (node) =>
      node.parentElement?.closest("button") ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT,
  });
  for (let node = walker.nextNode(); node; node = walker.nextNode()) nodes.push(node as Text);
  return nodes;
}

/** The text offset of a selection point (`container`, `offset`) in `nodes`. */
function offsetOf(nodes: Text[], container: Node, offset: number) {
  let before = 0;
  const probe = container.ownerDocument!.createRange();
  for (const node of nodes) {
    if (node === container) return before + Math.min(offset, node.length);
    probe.setStart(node, 0);
    // The point lies at or before this piece: everything before it counts.
    if (probe.comparePoint(container, offset) <= 0) return before;
    before += node.length;
  }
  return before;
}

/** The selection, when it lies wholly inside `root` and selects something. */
export function saveSelection(root: Element): ReplySelection | undefined {
  const selection = root.ownerDocument.getSelection();
  if (!selection || !selection.rangeCount || selection.isCollapsed) return undefined;
  const { anchorNode, focusNode } = selection;
  if (!anchorNode || !focusNode || !root.contains(anchorNode) || !root.contains(focusNode))
    return undefined;
  const nodes = textNodes(root);
  const anchor = offsetOf(nodes, anchorNode, selection.anchorOffset);
  const focus = offsetOf(nodes, focusNode, selection.focusOffset);
  const text = nodes
    .map((node) => node.data)
    .join("")
    .slice(Math.min(anchor, focus), Math.max(anchor, focus));
  return text ? { anchor, focus, text } : undefined;
}

/** Selects `saved` again in `root`; whether it could. */
export function restoreSelection(root: Element, saved: ReplySelection) {
  const nodes = textNodes(root);
  const plan = restorePlan(saved, nodes.map((node) => node.data));
  const selection = root.ownerDocument.getSelection();
  if (!plan || !selection) return false;
  selection.setBaseAndExtent(
    nodes[plan.anchor[0]],
    plan.anchor[1],
    nodes[plan.focus[0]],
    plan.focus[1],
  );
  return true;
}

/** How long a saved selection waits for its saved reply to be drawn. */
const PATIENCE_MS = 3000;

/**
 * Keeps the person's selection when a streaming reply is replaced by the
 * saved reply of the same text: `keep` saves it as the live reply goes, and
 * `drawn` selects it again in the saved reply once its Markdown is drawn.
 * Nothing is restored when the person did something in between (pressed,
 * typed or tapped, other than copying) or now types in a field, and a copy
 * meanwhile copies the text they had selected.
 */
export function createReplySelection(doc: Document = document) {
  let pending: { saved: ReplySelection; item: string; until: number } | undefined;
  /** `keep` is drawing the saved reply itself. */
  let keeping = false;
  const forget = () => {
    pending = undefined;
  };
  const acted = (event: Event) => {
    if (!pending) return;
    if (event instanceof KeyboardEvent) {
      const copy = (event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "c";
      if (copy || ["Meta", "Control", "Shift", "Alt"].includes(event.key)) return;
    }
    forget();
  };
  const copy = (event: ClipboardEvent) => {
    if (!pending || !event.clipboardData) return;
    const selection = doc.getSelection();
    if (selection && !selection.isCollapsed) return;
    event.clipboardData.setData("text/plain", pending.saved.text);
    event.preventDefault();
  };
  /** Selects the saved text in `host` when it is the saved reply. `now`: the
   * live reply, and the selection in it, are still there. */
  const take = (host: Element, now: boolean) => {
    if (!pending) return;
    if (Date.now() > pending.until) return forget();
    const row = host.closest("[data-item-id]");
    if (row?.getAttribute("data-item-id") !== pending.item) return;
    const { saved } = pending;
    forget();
    const selection = doc.getSelection();
    // What the person chose meanwhile stands.
    if (!now && selection && !selection.isCollapsed) return;
    if (isTextEntry(doc.activeElement)) return;
    restoreSelection(host, saved);
  };
  const events = ["pointerdown", "keydown", "touchstart"] as const;
  for (const name of events) doc.addEventListener(name, acted, true);
  doc.addEventListener("copy", copy, true);
  return {
    /** The live reply `root` is going away; its text becomes the saved reply
     * of item `item`. */
    keep(root: Element, item: string | undefined) {
      const saved = item ? saveSelection(root) : undefined;
      pending = saved && item ? { saved, item, until: Date.now() + PATIENCE_MS } : undefined;
      if (!pending) return;
      // The saved reply is drawn now, on this thread, and takes the selection
      // before the live reply leaves: no frame is drawn without it. A reply
      // not on screen yet takes it when it is drawn.
      const host = doc.querySelector(`[data-item-id="${CSS.escape(pending.item)}"] .markdown`);
      if (!host) return;
      keeping = true;
      try {
        host.dispatchEvent(new Event("markdown-draw-now"));
      } finally {
        keeping = false;
      }
      if (host.getAttribute("data-markdown-ready") === "true") take(host, true);
    },
    /** A Markdown host was drawn. */
    drawn(host: Element) {
      if (!keeping) take(host, false);
    },
    dispose() {
      forget();
      for (const name of events) doc.removeEventListener(name, acted, true);
      doc.removeEventListener("copy", copy, true);
    },
  };
}
