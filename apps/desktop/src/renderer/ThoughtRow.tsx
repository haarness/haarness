import { language, t } from "./i18n";
import { Show } from "solid-js";
import { Markdown } from "./Markdown";
import { Icon } from "./icons";
import { isOpen, setOpen } from "./disclosure";
import { thoughtDuration } from "./thinking";

/**
 * A finished thought in Compact: one step row, "Thought for 12s", on the
 * same rail as the tools, that opens in place to the text. While the model
 * thinks, the activity line ticks through its newest line instead, so the
 * transcript never grows or jumps with a thought.
 */
export function ThoughtRow(props: { id: string; text: string; durationMs?: number | null }) {
  const key = () => props.id + ":thought";
  const label = () =>
    props.durationMs == null
      ? t("Thought")
      : t("Thought for {duration}", { duration: thoughtDuration(props.durationMs, language()) });
  return (
    <details
      class="tool-call thought"
      data-status="done"
      open={isOpen(key())}
      onToggle={(event) => setOpen(key(), event.currentTarget.open)}
    >
      <summary>
        <span class="tool-mark" aria-hidden="true">
          <Icon name="reasoning" size="sm" />
        </span>
        <span class="tool-label">
          <strong>{label()}</strong>
        </span>
        <Icon name="chevron" size="xs" class="tool-chevron" />
      </summary>
      <Show when={isOpen(key())}>
        <div class="tool-body thought-body">
          <Markdown text={props.text} />
        </div>
      </Show>
    </details>
  );
}
