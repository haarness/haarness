import { createSignal } from "solid-js";
import { t } from "./i18n";
import type { Item, Thread } from "@haarness/sdk";
import { useApp } from "./state";
import { lastUserMessage } from "./conversation-history";
import { shortcutLabel, type SearchEntry } from "./search";

export const historyCommands = [
  { name: "undo", description: "Undo the last turn and restore its prompt" },
  { name: "redo", description: "Restore the next undone turn" },
  {
    name: "compact",
    description: "Summarize this chat with its model",
  },
];
export function createConversationControls(props: {
  submitting: () => boolean;
  restored: (item?: Item) => void;
  redone: () => void;
  openFind: () => void;
}) {
  const app = useApp();
  const thread = () =>
    app.state.threads.find((item) => item.id === app.state.selected);
  const owner = () => app.state.connection?.id + ":" + app.state.selected;
  const [pending, setPending] = createSignal<string[]>([]);
  const busy = () => pending().includes(owner());
  const disabled = (action: string) =>
    !app.state.ready ||
    !thread() ||
    thread()!.isSubagent ||
    busy() ||
    props.submitting() ||
    app.state.turns.some((turn) => turn.threadId === app.state.selected) ||
    (action === "redo"
      ? !thread()?.revert
      : action === "compact"
        ? !!thread()?.revert || !lastUserMessage(app.state.items)
        : !lastUserMessage(app.state.items, thread()?.revert?.sequence));
  async function invoke(action: string) {
    if (disabled(action))
      throw Error(
        action === "compact" && thread()?.revert
          ? "Redo or continue the conversation before compacting."
          : "This conversation action is unavailable while work is running or there is no eligible history.",
      );
    const id = app.state.selected;
    const connection = app.state.connection!.id;
    const key = owner();
    const before = [...app.state.items];
    setPending((values) => [...values, key]);
    try {
      const value = await window.haarness.api<Thread | Item>(
        connection,
        action === "undo"
          ? "revertThread"
          : action === "redo"
            ? "unrevertThread"
            : "compact",
        action === "redo" ? [id] : [id, {}],
      );
      if (owner() !== key) return;
      await app.refresh();
      if (owner() !== key) return;
      if (action === "undo") {
        const boundary = (value as Thread).revert?.sequence ?? -1;
        const undone = before.find(
          (item) =>
            item.sequence > boundary &&
            item.data.type === "user_message" &&
            !item.data.synthetic &&
            !item.data.queued,
        );
        props.restored(undone);
      } else if (action === "redo") props.redone();
      app.notify(
        action === "compact"
          ? t("Chat compacted")
          : action === "undo"
            ? t("Last turn undone")
            : t("Turn restored"),
      );
    } finally {
      setPending((values) => values.filter((value) => value !== key));
    }
  }
  app.registerSearch("conversation-history", (): SearchEntry[] =>
    !thread()
      ? []
      : [
          {
            key: "conversation:find",
            group: "Chat actions",
            name: t("Find in chat"),
            icon: "search",
            hint: shortcutLabel("Mod+F", app.state.platform),
            action: props.openFind,
          },
          ...(!thread()!.isSubagent
            ? historyCommands.map((command) => ({
                key: "conversation:" + command.name,
                group: "Chat actions",
                name:
                  command.name === "undo"
                    ? t("Undo last turn")
                    : command.name === "redo"
                      ? t("Redo turn")
                      : t("Compact chat"),
                detail: t(command.description),
                keywords: "/" + command.name,
                icon: (command.name === "undo"
                  ? "undo"
                  : command.name === "redo"
                    ? "redo"
                    : "shrink") as SearchEntry["icon"],
                disabled: disabled(command.name),
                action: () => app.run(() => invoke(command.name)),
              }))
            : []),
        ],
  );
  return { busy, disabled, invoke };
}
