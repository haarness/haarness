import type {
  WorkspaceOperationResponse,
  WorkspaceRefsResponse,
} from "@haarness/sdk";
import { createSignal } from "solid-js";
import type { useApp } from "./state";
import { ask, confirm } from "./ui";
import { t } from "./i18n";
import { primaryLocation, type DesktopProject } from "../projects";
import type { Recent } from "../shared";

export async function createWorktree(
  app: ReturnType<typeof useApp>,
  location?: string,
) {
  const origin = app.project()?.id;
  const connection = location
    ? await app.projectConnection(location)
    : app.state.connection;
  if (!connection || connection.remote) return false;
  const api = <T>(method: string, ...args: unknown[]) =>
    window.haarness.api<T>(connection.id, method, args);
  // The dialog opens at once. The list of bases fills in when the server
  // answers; a submit before then simply takes the server's default base,
  // which is the same branch the list would have preselected.
  const [bases, setBases] = createSignal<{ reference: string; label: string }[]>([]);
  void api<WorkspaceRefsResponse>("workspaceRefs")
    .then((refs) =>
      setBases(
        [...new Set([refs.defaultRef, ...refs.refs.map((ref) => ref.reference)])].map(
          (reference) => ({
            reference,
            label: reference.startsWith("refs/heads/")
              ? reference.slice(11)
              : reference.startsWith("refs/remotes/")
                ? "Remote: " + reference.slice(13)
                : reference.startsWith("refs/tags/")
                  ? "Tag: " + reference.slice(10)
                  : "Commit: " + reference.slice(0, 10),
          }),
        ),
      ),
    )
    .catch(() => setBases([]));
  const input = await ask(
    t("New worktree"),
    [
      {
        name: "name",
        label: t("Name"),
        placeholder: t("Feature or task name"),
        required: true,
      },
      {
        name: "fromRef",
        label: t("Start from"),
        type: "select",
        options: () => bases().map((base) => base.label),
      },
      {
        name: "branch",
        label: t("Branch name (optional)"),
        placeholder: t("Generated from the name"),
      },
    ],
    {
      description: t("Create a separate checkout for this project."),
      submit: t("Create worktree"),
    },
  );
  if (!input) return false;
  input.fromRef =
    bases().find((base) => base.label === input.fromRef)?.reference ?? "";
  const created = await api<WorkspaceOperationResponse>(
    "createWorkspace",
    Object.fromEntries(Object.entries(input).filter(([, value]) => value)),
  );
  await app.refreshProjects();
  // Creation belongs to the project captured when its menu was opened.
  // Background navigation must not redirect this operation into another repo.
  if (app.project()?.id !== origin) return false;
  return app.connect(
    { workspace: created.workspace.path, trust: connection.trust },
    true,
  );
}

// Always execute checkout management through its project's root connection.
// A sidebar menu can belong to a project other than the current conversation.
async function rootConnection(
  app: ReturnType<typeof useApp>,
  project: DesktopProject,
) {
  const root = primaryLocation(project);
  if (root.remote || root.workspace !== project.root)
    throw new Error("Open the project's root folder to manage its worktrees.");
  const connection = await app.projectConnection(root.id);
  if (!connection) throw new Error("The project is unavailable.");
  return connection;
}

export async function resetWorktree(
  app: ReturnType<typeof useApp>,
  project: DesktopProject,
  location: Recent,
) {
  if (!location.checkout?.managed) return;
  const connection = await rootConnection(app, project);
  const input = await ask(
    t("Reset worktree"),
    [
      { name: "toRef", label: t("Target ref (optional)") },
      {
        name: "discard",
        label: t("Uncommitted changes"),
        type: "select",
        options: [
          "Keep (refuse reset if dirty)",
          "Discard tracked changes",
          "Discard tracked and untracked changes",
        ],
      },
    ],
    {
      description: t("Reset “{name}”. Finish its tasks and close its running tools first.", {
        name: location.checkout.name,
      }),
      submit: t("Reset worktree"),
    },
  );
  if (!input) return;
  if (
    app.state.connection?.id === location.id &&
    !(await app.canLeaveWorkspace())
  )
    return;
  await window.haarness.api(connection.id, "resetWorkspace", [
    location.checkout.id,
    {
      confirm: true,
      discardChanges: input.discard.startsWith("Discard"),
      removeUntracked:
        input.discard === "Discard tracked and untracked changes",
      ...(input.toRef ? { toRef: input.toRef } : {}),
    },
  ]);
  await app.refreshProjects();
  app.set("catalogVersion", (value) => value + 1);
  app.notify(t("Worktree reset."));
}

export async function removeWorktree(
  app: ReturnType<typeof useApp>,
  project: DesktopProject,
  location: Recent,
) {
  if (!location.checkout?.managed) return;
  const connection = await rootConnection(app, project);
  if (
    !(await confirm(
      t("Remove worktree?"),
      location.checkout.exists
        ? t("Remove “{name}”. Dirty worktrees and running tasks are protected.", { name: location.checkout.name })
        : t("Remove the missing checkout's registration. Chat history is retained."),
      { submit: t("Remove worktree"), danger: true },
    ))
  )
    return;
  if (
    app.state.connection?.id === location.id &&
    !(await app.activateProject(connection.id))
  )
    return;
  await window.haarness.api(
    connection.id,
    location.checkout.exists ? "removeWorkspace" : "cleanupWorkspace",
    [location.checkout.id, { confirm: true }],
  );
  await window.haarness.forget(location.id);
  await app.refreshProjects();
  app.notify(t("Worktree removed."));
}
