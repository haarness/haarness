import { t } from "./i18n";
import type { Thread } from "@haarness/sdk";
import { useApp } from "./state";
import { ask, Button } from "./ui";
import { MenuButton } from "./MenuButton";

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
          t(props.thread.archived ? "Restore session {title}" : "Archive session {title}", { title: props.thread.title })
        }
        onClick={() =>
          void action(props.thread.archived ? "Unarchive" : "Archive")
        }
      />
    );
  return (
    <MenuButton
        icon="more"
        label={props.label ?? "Actions for " + props.thread.title}
        items={[
          ...(props.thread.id === app.state.selected &&
          (!props.connectionId ||
            props.connectionId === app.state.connection?.id)
            ? app
                .searchCommands()
                .filter(
                  (entry) =>
                    entry.key.startsWith("conversation:") &&
                    entry.key !== "conversation:compact",
                )
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
    t("Rename session"),
    [{ name: "title", label: t("Title"), value: thread.title, required: true }],
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
