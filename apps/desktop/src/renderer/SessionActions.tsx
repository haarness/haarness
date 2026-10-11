import { t } from "./i18n";
import type { Thread } from "@haarness/sdk";
import { useApp } from "./state";
import { ask, Button } from "./ui";
import { MenuButton } from "./MenuButton";
import { chatTitle, editableTitle } from "./chat-title";

export function SessionActions(props: {
  thread: Thread;
  connectionId?: string;
  label?: string;
  archiveOnly?: boolean;
}) {
  const app = useApp();
  const action = (name: string) =>
    app.run(async () => {
      const thread = props.thread;
      if (props.archiveOnly && props.connectionId) {
        await window.haarness.api(props.connectionId, "updateThread", [
          thread.id,
          { archived: name === "Archive" },
        ]);
        if (app.state.connection?.id === props.connectionId)
          await app.refresh();
        return;
      }
      if (
        props.connectionId &&
        !(await app.activateProject(props.connectionId))
      )
        return;
      if (name === "Rename") {
        await renameSession(app, thread);
        return;
      } else if (name === "Fork") {
        const fork = await app.api<Thread>("forkThread", thread.id, {});
        await app.refresh();
        await app.choose(fork.id);
      } else if (name === "Archive" || name === "Unarchive") {
        await app.api("updateThread", thread.id, {
          archived: name === "Archive",
        });
      }
      await app.refresh();
    });
  if (props.archiveOnly)
    return (
      <Button
        icon={props.thread.archived ? "restore" : "archive"}
        size="sm"
        label={
          t(props.thread.archived ? "Restore chat {title}" : "Archive chat {title}", { title: chatTitle(props.thread.title) })
        }
        onClick={() =>
          void action(props.thread.archived ? "Unarchive" : "Archive")
        }
      />
    );
  return (
    <MenuButton
        icon="more"
        label={props.label ?? t("Chat actions")}
        items={[
          ...(props.thread.id === app.state.selected &&
          (!props.connectionId ||
            props.connectionId === app.state.connection?.id)
            ? app
                .searchCommands()
                .filter((entry) => entry.key.startsWith("conversation:"))
                .map((entry) => ({
                  label: entry.name,
                  icon: entry.icon,
                  disabled: entry.disabled,
                  run: () => {
                    void entry.action();
                  },
                }))
            : []),
          { label: t("Rename"), icon: "edit", run: () => void action("Rename") },
          { label: t("Fork"), icon: "fork", run: () => void action("Fork") },
          {
            label: props.thread.archived ? "Unarchive" : "Archive",
            icon: props.thread.archived ? "restore" : "archive",
            run: () =>
              void action(props.thread.archived ? "Unarchive" : "Archive"),
          },
      ]}
    />
  );
}

export async function renameSession(
  app: ReturnType<typeof useApp>,
  thread: Thread,
) {
  const input = await ask(
    t("Rename chat"),
    [
      {
        name: "title",
        label: t("Title"),
        // Never the server's placeholder title ("New thread").
        value: editableTitle(thread.title),
        placeholder: t("Untitled chat"),
        required: true,
      },
    ],
    {
      // `required` accepts spaces, which would leave a blank row and header.
      onSubmit: async ({ title }) => {
        if (!title.trim()) throw new Error(t("Enter a title."));
        await app.api("updateThread", thread.id, { title: title.trim() });
      },
    },
  );
  if (!input) return;
  await app.refresh();
}
