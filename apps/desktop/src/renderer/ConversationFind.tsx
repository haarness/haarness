import { t } from "./i18n";
import { createEffect, createMemo, createSignal, on, Show } from "solid-js";
import type { Item } from "@haarness/sdk";
import { findConversationMessages } from "./conversation-history";
import { Button } from "./ui";
import "./conversation-controls.css";

export function ConversationFind(props: {
  rows: Item[];
  items: Item[];
  reveal: (id: string) => void;
  selected: (id: string) => void;
  close: () => void;
}) {
  const [query, setQuery] = createSignal("");
  const [index, setIndex] = createSignal(0);
  const matches = createMemo(() =>
    findConversationMessages(props.rows, props.items, query()),
  );
  const current = () =>
    matches()[Math.min(index(), Math.max(0, matches().length - 1))];
  createEffect(on(query, () => setIndex(0)));
  createEffect(() => setIndex((value) => Math.min(value, Math.max(0, matches().length - 1))));
  createEffect(
    on(
      [() => current()?.id, query],
      ([id]) => {
        props.selected(id ?? "");
        if (id) props.reveal(id);
      },
    ),
  );
  const move = (step: number) => {
    if (!matches().length) return;
    setIndex((value) => (value + step + matches().length) % matches().length);
    if (current()) props.reveal(current()!.id);
  };
  return (
    <div
      class="conversation-find"
      role="search"
      aria-label={t("Find in chat")}
    >
      <div class="conversation-find-controls">
        <input
          aria-label={t("Find in chat")}
          placeholder={t("Find in chat…")}
          type="search"
          ref={(element) => queueMicrotask(() => element.focus())}
          value={query()}
          onInput={(event) => setQuery(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              event.stopPropagation();
              props.close();
            }
            if (event.key === "Enter") {
              event.preventDefault();
              event.stopPropagation();
              move(event.shiftKey ? -1 : 1);
            }
          }}
        />
        <span role="status">
          {query().trim()
            ? matches().length
              ? t("{index} of {count} messages", { index: Math.min(index() + 1, matches().length), count: matches().length })
              : t("No matches")
            : t("Search messages and tool output")}
        </span>
        <Button
          icon="up"
          label={t("Previous match")}
          disabled={!matches().length}
          onClick={() => move(-1)}
        />
        <Button
          icon="down"
          label={t("Next match")}
          disabled={!matches().length}
          onClick={() => move(1)}
        />
        <Button
          icon="close"
          label={t("Close find in chat")}
          onClick={props.close}
        />
      </div>
      <Show when={current()}>
        {(hit) => (
          <div class="conversation-find-preview">
            {hit().before}
            <mark>{hit().match}</mark>
            {hit().after}
          </div>
        )}
      </Show>
    </div>
  );
}
