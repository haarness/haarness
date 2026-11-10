import { createEffect, createMemo, createSignal, on, onCleanup, onMount, Show, type JSX } from "solid-js";
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
    !article ||
    !row ||
    !item ||
    !article.matches('article[data-type="assistant_message"]') ||
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
  return { item, host, article, row, range: trimmed };
}

/** A length from the theme, in pixels. */
const token = (name: string) =>
  parseFloat(getComputedStyle(document.documentElement).getPropertyValue(name)) || 0;

/** Where the comment goes for `range` in reply `host` (in `article`, drawn in `row`). */
function place(range: Range, host: Element, article: Element, row: Element) {
  const lines = [...range.getClientRects()].filter((rect) => rect.width > 0);
  const first = lines[0];
  const last = lines.at(-1);
  if (!first || !last) return undefined;
  const end = range.cloneRange();
  end.collapse(false);
  const caret = end.getBoundingClientRect();
  const footer = article.querySelector(":scope > .message-footer");
  return commentSpot({
    first,
    last,
    caret: caret.height > 0 ? caret : undefined,
    reply: host.getBoundingClientRect(),
    row: row.getBoundingClientRect(),
    footer: footer?.getBoundingClientRect(),
    actions: footer?.querySelector(".message-tools")?.getBoundingClientRect(),
    rtl: document.documentElement.dir === "rtl",
    gap: token("--s0"),
    button: token("--control-sm"),
    space: token("--s2"),
  });
}

const sameSpot = (a: CommentSpot, b: CommentSpot) =>
  a.left === b.left &&
  a.width === b.width &&
  a.under === b.under &&
  a.over === b.over &&
  a.from === b.from &&
  a.backward === b.backward &&
  a.footer?.top === b.footer?.top &&
  a.footer?.from === b.footer?.from &&
  a.footer?.backward === b.footer?.backward;

/**
 * A strip as wide as the reply holding the button or the box, `from` one of
 * its edges. The strip is a flex row whose leading spacer is `from` wide but
 * may shrink: what follows it starts at the selection's end, and is pushed
 * back by as much as it would overhang the reply. `side` says where the
 * strip lies: `under` the selection's last line, `over` its first (what it
 * holds then ends just above the selection), or `at` exactly `top`.
 */
function Strip(props: {
  spot: CommentSpot;
  side: "under" | "over" | "at";
  top: number;
  from: number;
  backward: boolean;
  children: JSX.Element;
}) {
  return (
    <div
      class={"reply-comment-strip " + props.side}
      classList={{ backward: props.backward }}
      style={{
        top: props.top + "px",
        left: props.spot.left + "px",
        width: props.spot.width + "px",
      }}
    >
      <span class="reply-comment-gap" style={{ "flex-basis": props.from + "px" }} />
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
  /** A box opened (true) or closed (false). On closing, says whether the
   * transcript is about to move on to newer output. */
  writing?: (open: boolean) => boolean | void;
}) {
  const [offer, setOffer] = createSignal<Passage>();
  const [writing, setWriting] = createSignal<Passage & { text: string }>();
  let button: HTMLButtonElement | undefined;
  let card: HTMLElement | undefined;
  /** A pointer is down: the selection is still being made. */
  let pressed = false;
  /** The open box was already drawn once, and took the keyboard then. */
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
    const spot = quote ? place(found.range, found.host, found.article, found.row) : undefined;
    return spot && { item: found.item, range: found.range, quote, spot };
  }
  function track() {
    frame = 0;
    if (writing()) return;
    // During a press the selection is still changing: a press that clears it
    // takes the button away at once, and the button for what it selects
    // comes when the press ends.
    if (pressed) {
      if (document.getSelection()?.isCollapsed !== false) setOffer(undefined);
      return;
    }
    const next = read();
    // Focusing the button can clear the selection under it; the button then
    // stays for the passage it was offered for, until another is selected.
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
    const spot = found && place(found.range, found.host, found.article, found.row);
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
    options.writing?.(true);
  }
  /** Closes the box; whether the transcript moves on to newer output now. */
  function close() {
    if (!writing()) return false;
    setWriting(undefined);
    return !!options.writing?.(false);
  }
  /** Back to the words selected and the button, with the keyboard on it,
   * unless the transcript moves on to newer output as the box closes. */
  function cancel() {
    const passage = writing();
    const moving = close();
    const transcript = options.transcript();
    if (
      moving ||
      !passage ||
      passage.range.collapsed ||
      !transcript?.contains(passage.range.commonAncestorContainer)
    ) {
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
    close();
    document.getSelection()?.removeAllRanges();
    options.add(replyCommentNote(passage.quote, text));
  }

  const press = (event: PointerEvent) => {
    const target = event.target as Node | null;
    if (target && (button?.contains(target) || card?.contains(target))) return;
    pressed = true;
    // Pressing elsewhere closes a box with nothing written in it; a box
    // holding words waits for Add or Cancel.
    const open = writing();
    if (open && !open.text.trim()) close();
  };
  const release = () => {
    if (!pressed) return;
    pressed = false;
    schedule();
  };
  // The button is drawn after the reply's footer, so plain Tab order would
  // reach Copy and Fork first: Tab from selected words goes straight to it.
  // Tab typed in a text field is left to the field.
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
        close();
      },
      { defer: true },
    ),
  );

  /**
   * The box, under the selection when it fits in the visible transcript there
   * and over it otherwise, so opening it never scrolls the chat. Which side
   * is chosen once, as it is first drawn and before that frame is painted.
   */
  function Card(props: { passage: Passage & { text: string }; first: boolean }) {
    const [over, setOver] = createSignal(false);
    let element!: HTMLDivElement;
    onMount(() => {
      const view = options.transcript()?.getBoundingClientRect();
      const row = element.closest("[data-item-id]")?.getBoundingClientRect();
      if (!view || !row) return;
      const box = element.getBoundingClientRect();
      const height = box.height + token("--s0");
      const below = view.bottom - (row.top + props.passage.spot.under);
      const above = row.top + props.passage.spot.over - view.top;
      if (height > below && above > below) setOver(true);
    });
    return (
      <Strip
        spot={props.passage.spot}
        side={over() ? "over" : "under"}
        top={over() ? props.passage.spot.over : props.passage.spot.under}
        from={props.passage.spot.from}
        backward={props.passage.spot.backward}
      >
        <div
          class="reply-comment-card"
          ref={(value) => {
            element = value;
            card = value;
            onCleanup(() => {
              if (card === value) card = undefined;
            });
          }}
        >
          <InlineComment
            label={t("“{quote}”", { quote: quoteLine(props.passage.quote) })}
            inputLabel={t("Comment on the reply")}
            placeholder={t("Comment on these words…")}
            autofocus={props.first}
            value={props.passage.text}
            onInput={(text) => setWriting((current) => current && { ...current, text })}
            onCancel={cancel}
            onSubmit={submit}
          />
        </div>
      </Strip>
    );
  }

  /** The button or the box, in the row of reply `item`. */
  function Spot(props: { item: string | undefined }) {
    return (
      <>
        <Show when={props.item && offer()?.item === props.item ? offer() : undefined}>
          {(passage) => (
            <Strip
              spot={passage().spot}
              side={passage().spot.footer ? "at" : "under"}
              top={passage().spot.footer?.top ?? passage().spot.under}
              from={passage().spot.footer?.from ?? passage().spot.from}
              backward={passage().spot.footer?.backward ?? passage().spot.backward}
            >
              <button
                ref={(element) => {
                  button = element;
                  onCleanup(() => {
                    if (button === element) button = undefined;
                  });
                }}
                type="button"
                class="reply-comment-button"
                aria-label={t("Comment on the selected words")}
                // A press here must not clear the selected words or move the keyboard.
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
            </Strip>
          )}
        </Show>
        <Show when={props.item && writing()?.item === props.item ? writing() : undefined}>
          {(passage) => {
            // Only the first drawing of an open box takes the keyboard: one
            // drawn again as its row scrolls back waits to be clicked.
            const first = !drawn;
            drawn = true;
            return <Card passage={passage()} first={first} />;
          }}
        </Show>
      </>
    );
  }

  return {
    /** Whether the row of reply `item` holds the button or the box, which
     * then come over the rows after it. */
    holds: (item: string | undefined) => !!item && (offer()?.item === item || writing()?.item === item),
    /** A box is open and has the keyboard: the transcript holds its view
     * meanwhile, as it does under a held press. Once the keyboard is
     * elsewhere (a message sent from the message box), it follows again. */
    holding: () => !!writing() && !!card?.contains(document.activeElement),
    Spot,
  };
}
