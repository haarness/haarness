import { For, Show } from "solid-js";
import { Icon } from "./ui";
import { formatNumber, t } from "./i18n";

/**
 * The files a pending undo left as they are, as one quiet line under the
 * undo notice: their names on hover, and listed when it is opened.
 */
export function UnrestoredFiles(props: { names: string[]; count: number; more: number }) {
  const summary = () =>
    props.count === 1
      ? t("1 file wasn't restored")
      : t("{count} files weren't restored", { count: formatNumber(props.count) });
  return (
    <details class="undo-unrestored">
      <summary title={props.names.join("\n")}>
        <Icon name="chevron" size="xs" />
        {summary()}
      </summary>
      <ul>
        {/* A path reads left to right, where the list starts in either direction. */}
        <For each={props.names}>{(name) => <li><bdi dir="ltr">{name}</bdi></li>}</For>
        <Show when={props.more > 0}>
          <li class="more">{t("+{count} more", { count: formatNumber(props.more) })}</li>
        </Show>
      </ul>
      <p>{t("Undo leaves files that are ignored, outside the folder or over 2 MiB as they are.")}</p>
    </details>
  );
}
