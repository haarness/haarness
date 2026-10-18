import { t } from "./i18n";
import { createSignal, onCleanup, onMount } from "solid-js";

/**
 * A comment typed under what it is about: a diff line in Review, or lines of a
 * Markdown file's Preview. Enter adds it to the message, Shift+Enter starts a
 * new line, Escape cancels.
 */
export function InlineComment(props: {
  label: string;
  side?: "original" | "updated";
  /** The text box's name and placeholder; a diff line's by default. */
  inputLabel?: string;
  placeholder?: string;
  value: string;
  onInput: (value: string) => void;
  onCancel: () => void;
  onSubmit: (value: string) => Promise<void>;
}) {
  let input!: HTMLTextAreaElement;
  const [busy, setBusy] = createSignal(false);
  async function submit() {
    const value = props.value.trim();
    if (!value || busy()) return;
    setBusy(true);
    try {
      await props.onSubmit(value);
    } finally {
      setBusy(false);
    }
  }
  onMount(() => {
    const frame = requestAnimationFrame(() => {
      input.focus({ preventScroll: true });
      input.closest(".diff-comment")?.scrollIntoView({ block: "nearest" });
    });
    onCleanup(() => cancelAnimationFrame(frame));
  });
  return (
    <form
      class="diff-comment"
      data-side={props.side}
      aria-label={t("Comment on {target}", { target: props.label })}
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
      onKeyDown={(event) => {
        if (event.isComposing) return;
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          if (!busy()) props.onCancel();
        }
      }}
    >
      <label>
        <span class="diff-comment-label" title={props.label}>
          {props.label}
        </span>
        <textarea
          ref={input}
          rows={3}
          aria-label={props.inputLabel ?? t("Review comment")}
          placeholder={props.placeholder ?? t("Comment on this line…")}
          value={props.value}
          disabled={busy()}
          onInput={(event) => props.onInput(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.isComposing || event.key !== "Enter" || event.shiftKey)
              return;
            event.preventDefault();
            event.stopPropagation();
            void submit();
          }}
        />
      </label>
      <footer>
        <span class="diff-comment-hint">{t("Shift+Enter for a new line")}</span>
        <button type="button" disabled={busy()} onClick={props.onCancel}>
          {t("Cancel")}
        </button>
        <button
          type="submit"
          class="primary"
          disabled={busy() || !props.value.trim()}
        >
          {t("Add to message")}
        </button>
      </footer>
    </form>
  );
}
