import { For, Show } from "solid-js";
import type { UnrestoredFile } from "@haarness/sdk";
import { Icon } from "./ui";
import { formatNumber, t } from "./i18n";
import { unrestoredFiles, unrestoredReason } from "./conversation-history";

/** "1 file …" or "N files …" in the interface language. */
function counted(count: number, one: string, many: string) {
  return count === 1 ? t(one) : t(many, { count: formatNumber(count) });
}

/** One kind of file an undo left: a line that opens to list them. */
function Group(props: {
  files: UnrestoredFile[];
  summary: string;
  pressing: boolean;
  more: number;
}) {
  return (
    <details class="undo-unrestored" classList={{ pressing: props.pressing }}>
      <summary title={props.files.map((file) => file.path).join("\n")}>
        <Icon name="chevron" size="xs" />
        {props.summary}
      </summary>
      <ul>
        {/* A path reads left to right, where the list starts in either direction. */}
        <For each={props.files}>
          {(file) => (
            <li>
              <bdi dir="ltr">{file.path}</bdi>
              <Show when={!props.pressing && unrestoredReason(file)}>
                {(reason) => <span class="reason"> · {reason()}</span>}
              </Show>
            </li>
          )}
        </For>
        <Show when={props.more > 0}>
          <li>{t("+{count} more", { count: formatNumber(props.more) })}</li>
        </Show>
      </ul>
    </details>
  );
}

/**
 * The files a pending undo left as they are, under the undo notice: those
 * left for their size first, in the text colour with why, as the folder no
 * longer matches a checkpoint there; then, quietly, the ignored ones and those
 * outside the folder. Names show on hover and are listed when a line is
 * opened.
 */
export function UnrestoredFiles(props: ReturnType<typeof unrestoredFiles>) {
  return (
    <>
      <Show when={props.large.length > 0}>
        <Group
          files={props.large}
          summary={
            counted(props.large.length, "1 file left as it was", "{count} files left as they were") +
            " · " +
            t("too large for Undo")
          }
          pressing
          more={0}
        />
      </Show>
      <Show when={props.quiet.length + props.more > 0}>
        <Group
          files={props.quiet}
          summary={counted(
            props.quiet.length + props.more,
            "1 file wasn't restored",
            "{count} files weren't restored",
          )}
          pressing={false}
          more={props.more}
        />
      </Show>
    </>
  );
}
