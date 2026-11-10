import { createEffect, createMemo, createSignal, on, onCleanup, Show, type JSX } from "solid-js";
import { t } from "./i18n";
import { Icon } from "./icons";
import { InlineComment } from "./InlineComment";
import { isTextEntry } from "./text-entry";
import { commentSpot, quoteLine, replyCommentNote, replyQuote, type CommentSpot } from "./reply-comment";
import "./reply-comment.css";

/** Words selected in one finished reply, and where its comment goes. */
interface Passage {
  item: string;
  range: Range;
  quote: string;
  spot: CommentSpot;
}

const elementOf = (node: Node) => (node instanceof Element ? node : node.parentElement);

/** Whether the part of the page from one point to another holds no text. */
function blank(from: [Node, number], to: [Node, number]) {
  const between = document.createRange();
  between.setStart(...from);
  between.setEnd(...to);
  return !between.toString().trim();
}

/**
 * The finished reply `range` lies in, inside `transcript`, and the range
 * trimmed to the reply's text: both ends in the one reply, or an end just
 * past it with no text between (a triple click on its last paragraph ends
 * on the next line). A reply still being written is not a row yet, and has
 * none.
 */
function replyOf(range: Range, transcript: HTMLElement) {
  const host =
    elementOf(range.startContainer)?.closest(".markdown") ??
    elementOf(range.endContainer)?.closest(".markdown");
  const article = host?.parentElement;
  const row = article?.closest<HTMLElement>("[data-item-id]");
  const item = row?.getAttribute("data-item-id");
  if (
    !host ||
    !row ||
    !item ||
    !article?.matches('article[data-type="assistant_message"]') ||
    !transcript.contains(row)
  )
    return undefined;
  const trimmed = range.cloneRange();
  if (!host.contains(range.startContainer)) {
    if (!blank([range.startContainer, range.startOffset], [host, 0])) return undefined;
    trimmed.setStart(host, 0);
  }
  if (!host.contains(range.endContainer)) {
    if (!blank([host, host.childNodes.length], [range.endContainer, range.endOffset])) return undefined;
    trimmed.setEnd(host, host.childNodes.length);
  }
  return { item, host, row, range: trimmed };
}

/** Where the comment goes for `range` in `host`, the reply in `row`. */
function place(range: Range, host: Element, row: Element) {
  const lines = [...range.getClientRects()].filter((rect) => rect.width > 0);
  const line = lines.at(-1);
  if (!line) return undefined;
  const end = range.cloneRange();
  end.collapse(false);
  const caret = end.getBoundingClientRect();
  return commentSpot(
    line,
    caret.height > 0 ? caret : undefined,
    host.getBoundingClientRect(),
    row.getBoundingClientRect(),
    document.documentElement.dir === "rtl",
  );
}

const sameSpot = (a: CommentSpot, b: CommentSpot) =>
  a.top === b.top && a.left === b.left && a.width === b.width && a.from === b.from && a.backward === b.backward;

/** A band across the reply under the selection's last line: its spacer
 * gives way first, so what it holds starts at the selection's end and
 * slides back to stay inside the reply. */
function Place(props: { spot: CommentSpot; children: JSX.Element }) {
  return (
    <div
      class="reply-comment-place"
      classList={{ backward: props.spot.backward }}
      style={{
        top: props.spot.top + "px",
        left: props.spot.left + "px",
        width: props.spot.width + "px",
      }}
    >
      <span class="reply-comment-gap" style={{ "flex-basis": props.spot.from + "px" }} />
      {props.children}
    </div>
  );
}

/**
 * Comments on the words of a finished reply. Selecting text inside one reply
 * shows a small Comment button where the selection ends, and nothing
 * otherwise; it opens a comment box in the same place, and adding the
 * comment puts the quoted words and the comment in the message box, as a
 * Preview comment does. Nothing is kept about the comment afterwards.
 *
 * The button and the box are drawn in the reply's own row of the transcript
 * (`Spot`), so they scroll with it. Their state lives here, so a comment
 * being written survives its row being scrolled out and back.
 */
export function createReplyComment(options: {
  /** The transcript, which holds the replies. */
  transcript: () => HTMLElement | undefined;
  /** The chat shown: a comment belongs to it. */
  chat: () => string | undefined;
  /** Whether comments can go anywhere now (the chat has a message box). */
  enabled: () => boolean;
  /** Puts a comment's text in the message box. */
  add: (note: string) => void;
}) {
  const [offer, setOffer] = createSignal<Passage>();
  const [writing, setWriting] = createSignal<Passage & { text: string }>();
  let button: HTMLButtonElement | undefined;
  let card: HTMLElement | undefined;
  /** A pointer is down: the selection is still being made. */
  let pressed = false;
  /** The box being written has been drawn and took the keyboard once. */
  let drawn = false;
  let frame = 0;

  /** The selection, when it is words of one finished reply. */
  function read(): Passage | undefined {
    const transcript = options.transcript();
    const selection = document.getSelection();
    if (!transcript || !selection || selection.isCollapsed || !selection.rangeCount || !options.enabled())
      return undefined;
    const found = replyOf(selection.getRangeAt(0), transcript);
    if (!found) return undefined;
    const quote = replyQuote(selection.toString());
    const spot = quote ? place(found.range, found.host, found.row) : undefined;
    return spot && { item: found.item, range: found.range, quote, spot };
  }
  function track() {
    frame = 0;
    if (writing()) return;
    // While a pointer is down the selection is still being made: the button
    // goes once a press leaves nothing selected, and comes when it is let go.
    if (pressed) {
      if (document.getSelection()?.isCollapsed !== false) setOffer(undefined);
      return;
    }
    const next = read();
    // While the button has the keyboard it keeps the words it was shown for
    // (the selection may go as it takes the focus), unless others are selected.
    if (!next && button && document.activeElement === button) return;
    const current = offer();
    if (next && current && next.item === current.item && next.quote === current.quote && sameSpot(next.spot, current.spot))
      return;
    setOffer(next);
  }
  const schedule = () => {
    if (!frame) frame = requestAnimationFrame(track);
  };
  /** The reply's text moved (an image came in, the window was resized). */
  function reflow() {
    const open = writing();
    if (!open) return schedule();
    const found = !open.range.collapsed && options.transcript() && replyOf(open.range, options.transcript()!);
    const spot = found && place(found.range, found.host, found.row);
    if (spot && !sameSpot(spot, open.spot)) setWriting({ ...open, spot });
  }
  const resized = new ResizeObserver(() => reflow());
  const watched = createMemo(() => (writing() ?? offer())?.item);
  createEffect(() => {
    resized.disconnect();
    const item = watched();
    const host = item && options.transcript()?.querySelector(`[data-item-id="${CSS.escape(item)}"] .markdown`);
    if (host) resized.observe(host);
  });

  function open() {
    const passage = offer();
    if (!passage) return;
    drawn = false;
    setWriting({ ...passage, text: "" });
    setOffer(undefined);
  }
  /** Back to the words selected and the button, with the keyboard on it. */
  function cancel() {
    const passage = writing();
    setWriting(undefined);
    const transcript = options.transcript();
    if (!passage || passage.range.collapsed || !transcript?.contains(passage.range.commonAncestorContainer)) {
      transcript?.focus({ preventScroll: true });
      return;
    }
    const selection = document.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(passage.range);
    setOffer(read());
    requestAnimationFrame(() => {
      if (button?.isConnected) button.focus({ preventScroll: true });
      else transcript.focus({ preventScroll: true });
    });
  }
  async function submit(text: string) {
    const passage = writing();
    if (!passage) return;
    setWriting(undefined);
    document.getSelection()?.removeAllRanges();
    options.add(replyCommentNote(passage.quote, text));
  }

  const press = (event: PointerEvent) => {
    const target = event.target as Node | null;
    if (target && (button?.contains(target) || card?.contains(target))) return;
    pressed = true;
    // A box left empty goes when the pointer goes elsewhere; one with words
    // in it stays until it is added or cancelled.
    const open = writing();
    if (open && !open.text.trim()) setWriting(undefined);
  };
  const release = () => {
    if (!pressed) return;
    pressed = false;
    schedule();
  };
  // Tab from words selected in a reply goes to their Comment button, which
  // comes after the reply's own links and buttons; a field keeps its Tab.
  const tab = (event: KeyboardEvent) => {
    if (event.key !== "Tab" || event.shiftKey || event.altKey || event.metaKey || event.ctrlKey) return;
    if (!offer() || writing() || !button?.isConnected) return;
    const focused = document.activeElement;
    const transcript = options.transcript();
    if (focused === button || isTextEntry(focused)) return;
    if (focused && focused !== document.body && !transcript?.contains(focused)) return;
    event.preventDefault();
    button.focus({ preventScroll: true });
  };
  document.addEventListener("selectionchange", schedule);
  document.addEventListener("pointerdown", press, true);
  window.addEventListener("pointerup", release, true);
  window.addEventListener("pointercancel", release, true);
  window.addEventListener("blur", release);
  document.addEventListener("keydown", tab, true);
  onCleanup(() => {
    document.removeEventListener("selectionchange", schedule);
    document.removeEventListener("pointerdown", press, true);
    window.removeEventListener("pointerup", release, true);
    window.removeEventListener("pointercancel", release, true);
    window.removeEventListener("blur", release);
    document.removeEventListener("keydown", tab, true);
    if (frame) cancelAnimationFrame(frame);
    resized.disconnect();
  });
  // A comment belongs to the chat it was started in.
  createEffect(
    on(
      options.chat,
      () => {
        setOffer(undefined);
        setWriting(undefined);
      },
      { defer: true },
    ),
  );

  /** The button or the box, in the row of reply `item`. */
  function Spot(props: { item: string | undefined }) {
    return (
      <>
        <Show when={props.item && offer()?.item === props.item ? offer() : undefined}>
          {(passage) => (
            <Place spot={passage().spot}>
              <button
                ref={(element) => {
                  button = element;
                  onCleanup(() => {
                    if (button === element) button = undefined;
                  });
                }}
                type="button"
                class="reply-comment-button"
                // The press must leave the selection, and the keyboard, where they are.
                onPointerDown={(event) => event.preventDefault()}
                onMouseDown={(event) => event.preventDefault()}
                onClick={open}
                onKeyDown={(event) => {
                  if (event.key !== "Escape") return;
                  event.preventDefault();
                  event.stopPropagation();
                  setOffer(undefined);
                  document.getSelection()?.removeAllRanges();
                  options.transcript()?.focus({ preventScroll: true });
                }}
                onBlur={() => queueMicrotask(schedule)}
              >
                <Icon name="chat" size="sm" />
                {t("Comment")}
              </button>
            </Place>
          )}
        </Show>
        <Show when={props.item && writing()?.item === props.item ? writing() : undefined}>
          {(passage) => {
            // Only when it opens does the box take the keyboard: scrolled
            // back into view, it waits to be clicked.
            const first = !drawn;
            drawn = true;
            return (
              <Place spot={passage().spot}>
                <div
                  class="reply-comment-card"
                  ref={(element) => {
                    card = element;
                    onCleanup(() => {
                      if (card === element) card = undefined;
                    });
                  }}
                >
                  <InlineComment
                    label={t("“{quote}”", { quote: quoteLine(passage().quote) })}
                    inputLabel={t("Comment on the reply")}
                    placeholder={t("Comment on these words…")}
                    autofocus={first}
                    value={passage().text}
                    onInput={(text) => setWriting((current) => current && { ...current, text })}
                    onCancel={cancel}
                    onSubmit={submit}
                  />
                </div>
              </Place>
            );
          }}
        </Show>
      </>
    );
  }

  return {
    /** Whether the row of reply `item` holds the button or the box, which
     * then come over the rows after it. */
    holds: (item: string | undefined) => !!item && (offer()?.item === item || writing()?.item === item),
    Spot,
  };
}
