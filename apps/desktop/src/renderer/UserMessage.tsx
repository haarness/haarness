import { t, formatNumber } from "./i18n";
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
  type JSX,
} from "solid-js";
import { Markdown } from "./Markdown";
import { Icon } from "./icons";
import { Button } from "./ui";
import { pasteLabel } from "./PromptContext";
import { commandPrompt, countLines, sentParts } from "./prompt-context";
import { isOpen, setOpen } from "./disclosure";
import "./user-message.css";

/** A prompt taller than this many lines folds to its first eight
 * (`user-message.css`), as the TUI folds one (`view/transcript.rs`), so a
 * fold always hides enough to be worth a click. */
const FOLD_LINES = 12;

/**
 * The words of the user's bubble. A slash command reads as the line typed,
 * with the prompt it expanded to (what the model received) folded under
 * it. A paste sent as a chip reads as one folded block under its label. A
 * long message folds after a few lines with a fade and Show more.
 */
export function UserMessageBody(props: {
  id: string;
  text: string;
  commandLine?: string | null;
  /** The message is the current Find match: everything in it opens. */
  revealed?: boolean;
  /** Lines of the prompt a command sent just now will expand to. */
  pendingPromptLines?: number;
  children?: JSX.Element;
}) {
  const shown = () => props.commandLine || props.text;
  const parts = createMemo(() => sentParts(shown()));
  const prompt = createMemo(() =>
    props.commandLine ? commandPrompt(props.commandLine, props.text) : undefined,
  );
  /** The "Show prompt" row: the prompt once the command has expanded, or,
   * while it runs, the size its template promises. */
  const promptRow = createMemo(() => {
    const text = prompt();
    if (text !== undefined) return { text, lines: Math.max(1, countLines(text)) };
    return props.pendingPromptLines ? { text: undefined, lines: props.pendingPromptLines } : undefined;
  });
  const foldKey = () => props.id + ":fold";
  const pasteKey = (index: number) => `${props.id}:paste:${index}`;
  const promptKey = () => props.id + ":prompt";
  const [tall, setTall] = createSignal(false);
  const folded = () => tall() && !isOpen(foldKey());
  createEffect(() => {
    if (!props.revealed) return;
    setOpen(foldKey(), true);
    parts().forEach((part, index) => {
      if (part.kind === "paste") setOpen(pasteKey(index), true);
    });
  });
  let body!: HTMLDivElement;
  let content!: HTMLDivElement;
  onMount(() => {
    // Measured from the unclamped content, before the frame is painted, so a
    // long message never shows unfolded first. An image is a thumbnail that
    // opens full size, so it does not make a message long: only the line it
    // sits on counts.
    const measure = () => {
      const line = parseFloat(getComputedStyle(body).lineHeight) || 22;
      let pictures = 0;
      for (const picture of content.querySelectorAll("img, .project-image"))
        pictures += Math.max(0, picture.getBoundingClientRect().height - line);
      setTall(content.scrollHeight - pictures > line * FOLD_LINES + 1);
    };
    const observer = new ResizeObserver(measure);
    observer.observe(content);
    onCleanup(() => observer.disconnect());
  });
  const [copied, setCopied] = createSignal(false);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(copiedTimer));
  async function copyPrompt(text: string) {
    await navigator.clipboard.writeText(text);
    setCopied(true);
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => setCopied(false), 1600);
  }
  return (
    <>
      <div
        ref={body}
        class="bubble-body"
        classList={{ folded: folded(), command: !!props.commandLine }}
      >
        <div ref={content} class="bubble-content">
          <For each={parts()}>
            {(part, index) =>
              part.kind === "text" ? (
                <Show when={part.text.trim()}>
                  <Markdown text={part.text} />
                </Show>
              ) : (
                <div class="sent-paste" classList={{ open: isOpen(pasteKey(index())) }}>
                  <button
                    type="button"
                    class="sent-paste-toggle"
                    aria-expanded={isOpen(pasteKey(index()))}
                    onClick={() => {
                      const open = !isOpen(pasteKey(index()));
                      setOpen(pasteKey(index()), open);
                      // A paste opened inside a folded message opens it too.
                      if (open) setOpen(foldKey(), true);
                    }}
                  >
                    <Icon name="chevron" size="xs" class="sent-paste-chevron" />
                    <span>{pasteLabel(part.text)}</span>
                  </button>
                  <Show when={isOpen(pasteKey(index()))}>
                    <pre class="sent-paste-text">{part.text}</pre>
                  </Show>
                </div>
              )
            }
          </For>
        </div>
      </div>
      <Show when={tall()}>
        <button
          type="button"
          class="bubble-more"
          aria-expanded={!folded()}
          onClick={() => {
            const open = folded();
            setOpen(foldKey(), open);
            // Show less returns the whole message to its short form.
            if (!open)
              parts().forEach((part, index) => {
                if (part.kind === "paste") setOpen(pasteKey(index), false);
              });
          }}
        >
          {folded() ? t("Show more") : t("Show less")}
        </button>
      </Show>
      {props.children}
      <Show when={promptRow()}>
        {(row) => (
          <div class="command-prompt" classList={{ open: isOpen(promptKey()) && !!row().text }}>
            <div class="command-prompt-bar">
              {/* Held from the moment the command is sent, so the row is
                  there before the prompt it opens arrives. */}
              <button
                type="button"
                class="command-prompt-toggle"
                aria-expanded={isOpen(promptKey()) && !!row().text}
                disabled={row().text === undefined}
                onClick={() => setOpen(promptKey(), !isOpen(promptKey()))}
              >
                <Icon name="chevron" size="xs" class="command-prompt-chevron" />
                <span>
                  {isOpen(promptKey()) && row().text
                    ? t("Hide prompt")
                    : row().lines === 1
                      ? t("Show prompt · 1 line")
                      : t("Show prompt · {count} lines", { count: formatNumber(row().lines) })}
                </span>
              </button>
              <Show when={isOpen(promptKey()) && row().text}>
                {(text) => (
                  <Button
                    icon={copied() ? "check" : "copy"}
                    size="sm"
                    label={copied() ? t("Copied") : t("Copy prompt")}
                    onClick={() => void copyPrompt(text())}
                  />
                )}
              </Show>
            </div>
            <Show when={isOpen(promptKey()) && row().text}>
              {(text) => (
                <div class="command-prompt-text">
                  <Markdown text={text()} />
                </div>
              )}
            </Show>
          </div>
        )}
      </Show>
    </>
  );
}
