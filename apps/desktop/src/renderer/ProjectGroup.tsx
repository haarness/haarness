import { t } from "./i18n";
import { splitSchedules } from "./schedule-data";
import { conversationRoot } from "../thread-kinds";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import {
  type DesktopProject,
  groupProjects,
  isWorktree,
  locationName,
  primaryLocation,
  projectKey,
  projectSections,
  sessionKey,
} from "../projects";
import type { Recent } from "../shared";
import { MenuButton, type MenuAction } from "./MenuButton";
import { createWorktree, resetWorktree, removeWorktree } from "./worktrees";
import { useApp } from "./state";
import { createOrdering, createOrderDropTarget } from "./Sortable";
import { Button, Icon, ask, confirm, type IconName } from "./ui";
import { SessionActions } from "./SessionActions";
import { createFileDrop } from "./file-drop";

export function ProjectGroup(props: {
  project: DesktopProject;
  order: ReturnType<typeof createOrdering>;
}) {
  const app = useApp();
  const { state } = app;
  const [showUnavailable, setShowUnavailable] = createSignal(false);
  const unavailable = () =>
    props.project.locations.filter(
      (location) => isWorktree(location) && location.checkout?.exists === false,
    );
  const active = () => app.currentProject()?.id === props.project.id;
  const expanded = () => state.expandedProjects[props.project.id] ?? false;
  const meta = () => state.projectMeta[props.project.id] ?? {};
  const pinned = () => !!meta().pinned;
  /** The label the catalog gives this project before any custom name. */
  const defaultLabel = () =>
    groupProjects(props.project.locations)[0]?.label ?? props.project.label;
  const revealLabel = () =>
    state.platform === "darwin"
      ? t("Reveal in Finder")
      : state.platform === "win32"
        ? t("Show in Explorer")
        : t("Show in file manager");
  const setSection = (section: string) =>
    app.updateProjectMeta(props.project.id, { section });
  /** Existing sections as alternatives, then a way to start a new one. */
  const sectionItems = (): MenuAction[] => {
    const sections = projectSections(state.projectMeta);
    const current = meta().section ?? "";
    return [
      ...sections.map((name) => ({
        label: name,
        checked: current === name,
        run: () => void app.run(() => setSection(name)),
      })),
      ...(sections.length
        ? [
            {
              label: t("No section"),
              checked: !current,
              run: () => void app.run(() => setSection("")),
            },
          ]
        : []),
      {
        label: t("New section…"),
        icon: "plus",
        divider: sections.length > 0,
        run: () =>
          void app.run(async () => {
            const input = await ask(
              t("New section"),
              [{ name: "name", label: t("Name"), required: true }],
              { submit: t("Create") },
            );
            if (input) await setSection(input.name);
          }),
      },
    ];
  };
  async function editProject() {
    const input = await ask(
      t("Edit project"),
      [
        {
          name: "name",
          label: t("Name"),
          value: props.project.label,
          placeholder: defaultLabel(),
        },
        {
          name: "root",
          label: t("Folder"),
          value: props.project.root,
          readonly: true,
        },
      ],
      {
        description: t("An empty name restores the folder name."),
        submit: t("Save"),
      },
    );
    if (!input) return;
    const name = input.name.trim();
    await app.updateProjectMeta(props.project.id, {
      name: name === defaultLabel() ? "" : name,
    });
  }
  const grouped = () =>
    props.project.git &&
    !props.project.remote &&
    !!state.worktreeViews[props.project.id];
  const locations = () => {
    const root = primaryLocation(props.project);
    return [
      root,
      ...props.project.locations.filter((location) => location.id !== root.id),
    ].filter(
      (location) =>
        showUnavailable() ||
        !unavailable().some((item) => item.id === location.id),
    );
  };
  app.registerSearch("project:" + props.project.id, () =>
    unavailable().length && grouped()
      ? [
          {
            key: "unavailable:" + props.project.id,
            group: "Worktree actions",
            name: showUnavailable()
              ? t("Hide unavailable worktrees")
              : t("Show unavailable worktrees"),
            detail: props.project.label,
            icon: "branch",
            action: () => {
              setShowUnavailable((value) => !value);
              return app.expandProject(props.project.id, true);
            },
          },
        ]
      : [],
  );
  const drop = () =>
    props.order.dropProps(
      props.project.id,
      t("project {name}", { name: props.project.label }),
      "y",
      ".project-row",
      {
        accepts: (source) =>
          (state.projectMeta[source]?.section ?? "") !== (meta().section ?? ""),
        receive: (source, position) =>
          void app.run(() => app.fileProjectBeside(source, props.project.id, position)),
      },
    );
  return (
    <section
      class="project-group"
      classList={{ current: active() }}
      aria-label={t("Project {name}", { name: props.project.label })}
      {...drop()}
    >
      <div class="project-row">
        <button
          class="project-disclosure"
          aria-label={
            expanded()
              ? t("Collapse project {name}", { name: props.project.label })
              : t("Expand project {name}", { name: props.project.label })
          }
          aria-expanded={expanded()}
          aria-controls={"sessions-" + props.project.id}
          onClick={() =>
            app.run(() => app.expandProject(props.project.id, !expanded()))
          }
        >
          <Icon name={expanded() ? "down" : "chevron"} size="xs" />
        </button>
        <button
          class="project-heading"
          classList={{ current: active() }}
          aria-expanded={expanded()}
          aria-controls={"sessions-" + props.project.id}
          aria-busy={props.project.locations.some(
            (location) => location.id === state.openingProject,
          )}
          title={props.project.root}
          aria-description={pinned() ? t("Pinned") : undefined}
          onClick={() =>
            app.run(async () => {
              await app.expandProject(props.project.id, !expanded());
              if (await app.selectProject(props.project.id))
                app.set("panel", "chat");
            })
          }
        >
          <span>{props.project.label}</span>
          <Show when={pinned()}>
            <Icon name="pin" size="xs" class="project-pin" />
          </Show>
        </button>
        <MenuButton
          label={t("Project options for {name}", { name: props.project.label })}
          icon="more"
          items={[
            {
              label: pinned() ? t("Unpin") : t("Pin"),
              icon: pinned() ? "unpin" : "pin",
              run: () =>
                void app.run(() =>
                  app.updateProjectMeta(props.project.id, {
                    pinned: !pinned(),
                  }),
                ),
            },
            {
              label: t("Edit"),
              icon: "settings",
              run: () => void app.run(editProject),
            },
            {
              divider: true,
              label: t("Section"),
              icon: "section",
              submenu: sectionItems(),
            },
            ...(props.project.remote
              ? []
              : [
                  {
                    label: revealLabel(),
                    icon: "folder-open" as const,
                    run: () =>
                      void app.run(() => app.revealProject(props.project.id)),
                  },
                ]),
            {
              divider: true,
              label: t("New chat"),
              icon: "plus",
              run: () =>
                void app.run(() =>
                  app.startChat(primaryLocation(props.project).id),
                ),
            },
            ...(props.project.git && !props.project.remote
              ? [
                  {
                    label: grouped()
                      ? t("Disable worktree view")
                      : t("Enable worktree view"),
                    icon: "branch" as const,
                    run: () =>
                      void app.run(() =>
                        app.setWorktreeView(props.project.id, !grouped()),
                      ),
                  },
                ]
              : []),
            ...(grouped() && unavailable().length
              ? [
                  {
                    label: showUnavailable()
                      ? t("Hide unavailable worktrees")
                      : t("Show unavailable worktrees ({count})", {
                          count: unavailable().length,
                        }),
                    icon: "branch" as const,
                    run: () => {
                      setShowUnavailable((value) => !value);
                      void app.run(() =>
                        app.expandProject(props.project.id, true),
                      );
                    },
                  },
                ]
              : []),
            {
              label: t("Refresh project"),
              icon: "refresh",
              run: () => void app.run(() => app.refreshProjects()),
            },
            {
              divider: true,
              label: t("Archive chats"),
              icon: "archive",
              run: () =>
                void app.run(async () => {
                  if (
                    await confirm(
                      t("Archive all chats in {name}?", {
                        name: props.project.label,
                      }),
                      t(
                        "Archived chats leave the sidebar. Search still finds them, and each one can be restored.",
                      ),
                    )
                  )
                    await app.archiveProjectChats(props.project.id);
                }),
            },
            {
              divider: true,
              label: t("Remove project"),
              icon: "close" as const,
              run: () =>
                void app.run(async () => {
                  if (
                    await confirm(
                      t("Remove this project from the sidebar?"),
                      t(
                        "Conversation history, running tasks and the saved server connection are preserved.",
                      ),
                    )
                  )
                    await app.forgetProject(props.project.id);
                }),
            },
          ]}
        />
      </div>
      <Show when={expanded()}>
        <Show
          when={grouped()}
          fallback={<SessionList project={props.project} />}
        >
          <div class="worktree-list" id={"sessions-" + props.project.id}>
            <button
              class="new-worktree"
              onClick={() =>
                app.run(() =>
                  createWorktree(app, primaryLocation(props.project).id),
                )
              }
            >
              <Icon name="plus" size="sm" />
              {t("New worktree")}
            </button>
            <For each={locations()}>
              {(location) => (
                <LocationGroup project={props.project} location={location} />
              )}
            </For>
          </div>
        </Show>
      </Show>
    </section>
  );
}

/** The projects of one group. They share the list-wide order but move among
 * themselves: Alt+arrows and drops step over the group's own rows only, and
 * a drop from another group is filed by the row it lands on. */
export function ProjectRows(props: { ids: string[] }) {
  const app = useApp();
  // Pins form a separate visible lane. Stepping through the raw saved order
  // can otherwise move past an invisible pinned slot and appear to do nothing.
  const order = createOrdering(
    () => "list:projects",
    () => props.ids.filter((id) => !app.state.projectMeta[id]?.pinned),
  );
  const pinnedOrder = createOrdering(
    () => "list:projects",
    () => props.ids.filter((id) => app.state.projectMeta[id]?.pinned),
  );
  return (
    <For each={props.ids}>
      {(id) => (
        <ProjectGroup
          project={app.projects().find((item) => item.id === id)!}
          order={app.state.projectMeta[id]?.pinned ? pinnedOrder : order}
        />
      )}
    </For>
  );
}

/** A named group of projects in the sidebar. The heading's menu shares the
 * trailing column with the per-project menus below it; the `fixed` group of
 * unfiled projects has a heading but nothing to rename or remove. */
export function ProjectSection(props: {
  name: string;
  ids: string[];
  fixed?: boolean;
}) {
  const app = useApp();
  const drop = createOrderDropTarget(
    () => "list:projects",
    () => "section:" + (props.fixed ? "" : props.name),
    (source) => void app.run(() => app.fileProjectInSection(source, props.fixed ? "" : props.name)),
  );
  return (
    <section
      class="project-section"
      aria-label={t("Section {name}", { name: props.name })}
      {...drop}
    >
      <div class="project-section-row" data-drop-position={drop["data-drop-position"]}>
        <span>{props.name}</span>
        <Show when={!props.fixed}>
        <MenuButton
          label={t("Section options for {name}", { name: props.name })}
          icon="more"
          items={[
            {
              label: t("Rename section…"),
              icon: "edit",
              run: () =>
                void app.run(async () => {
                  const input = await ask(
                    t("Rename section"),
                    [
                      {
                        name: "name",
                        label: t("Name"),
                        value: props.name,
                        required: true,
                      },
                    ],
                    { submit: t("Save") },
                  );
                  if (input) await app.renameSection(props.name, input.name);
                }),
            },
            {
              label: t("Remove section"),
              icon: "close",
              run: () =>
                void app.run(async () => {
                  if (
                    await confirm(
                      t("Remove section {name}?", { name: props.name }),
                      t("Its projects stay in the sidebar without a section."),
                    )
                  )
                    await app.removeSection(props.name);
                }),
            },
          ]}
        />
        </Show>
      </div>
      <ProjectRows ids={props.ids} />
    </section>
  );
}

function LocationGroup(props: { project: DesktopProject; location: Recent }) {
  const app = useApp();
  const name = () => t(locationName(props.location));
  const expanded = () => app.state.expandedLocations[props.location.id] ?? true;
  const missing = () => props.location.checkout?.exists === false;
  const current = () => app.project()?.id === props.location.id;
  return (
    <section
      class="worktree-group"
      aria-label={t("Checkout {name}", { name: name() })}
      classList={{ current: current(), missing: missing() }}
    >
      <div class="worktree-row">
        <button
          class="worktree-heading"
          aria-label={
            expanded()
              ? t("Collapse checkout {name}", { name: name() })
              : t("Expand checkout {name}", { name: name() })
          }
          aria-expanded={expanded()}
          aria-controls={"sessions-" + props.location.id}
          title={props.location.workspace}
          onClick={() =>
            app.run(() => app.expandLocation(props.location.id, !expanded()))
          }
        >
          <Icon name={expanded() ? "down" : "chevron"} size="xs" />
          <Icon
            name={isWorktree(props.location) ? "branch" : "folder"}
            size="sm"
          />
          <span class="worktree-label">
            <span>
              {props.location.workspace === props.project.root
                ? t("Local")
                : name()}
            </span>
            <small>
              {missing()
                ? t("Folder not found")
                : (props.location.checkout?.branch ?? name())}
            </small>
          </span>
        </button>
        <MenuButton
          label={t("Checkout options for {name}", { name: name() })}
          icon="more"
          items={[
            {
              label: t("Open checkout"),
              icon: "folder",
              disabled: missing(),
              run: () =>
                void app.run(async () => {
                  if (await app.activateProject(props.location.id)) {
                    await app.expandLocation(props.location.id, true);
                    app.set("panel", "chat");
                  }
                }),
            },
            ...(props.location.checkout?.managed
              ? [
                  {
                    label: t("Reset worktree…"),
                    icon: "refresh" as const,
                    disabled: missing(),
                    run: () =>
                      void app.run(() =>
                        resetWorktree(app, props.project, props.location),
                      ),
                  },
                  {
                    label: missing()
                      ? t("Remove missing worktree…")
                      : t("Remove worktree…"),
                    icon: "trash" as const,
                    run: () =>
                      void app.run(() =>
                        removeWorktree(app, props.project, props.location),
                      ),
                  },
                ]
              : []),
          ]}
        />
        <Button
          icon="plus"
          label={t("New chat in {name}", { name: name() })}
          disabled={missing()}
          onClick={() => app.run(() => app.startChat(props.location.id))}
        />
      </div>
      <Show when={expanded()}>
        <Show
          when={!missing()}
          fallback={
            <p class="sessions-empty">
              {t(
                "Restore the folder at its original path to reopen these conversations.",
              )}
            </p>
          }
        >
          <SessionList project={props.project} location={props.location} />
        </Show>
      </Show>
    </section>
  );
}

/* Without a project this lists every conversation the sidebar can see, which
   is what the "In one list" organisation renders. */
export function SessionList(props: {
  project?: DesktopProject;
  location?: Recent;
}) {
  const app = useApp();
  const { state } = app;
  const rows = () =>
    app
      .conversations(props.project?.id)
      .filter(
        ({ thread, location }) =>
          !thread.archived &&
          !thread.isSubagent &&
          location.checkout?.exists !== false &&
          (!props.location || location.id === props.location.id),
      )
      .sort(
        (a, b) =>
          b.thread.createdAt.localeCompare(a.thread.createdAt) ||
          a.key.localeCompare(b.key),
      );
  const split = createMemo(() => splitSchedules(rows()));
  const order = createOrdering(
    () =>
      "list:sessions:" +
      (props.location?.id ?? props.project?.id ?? "all"),
    () => split().chats.map((row) => row.key),
    "start",
  );
  const availableLocations = () =>
    (props.location
      ? [props.location]
      : (props.project?.locations ??
        app.projects().flatMap((item) => item.locations))
    ).filter(
      (location) => location.checkout?.exists !== false,
    );
  const loading = () =>
    availableLocations().some(
      (location) => app.indexes()[location.id]?.loading,
    );
  const failures = () =>
    availableLocations().filter(
      (location) => !!app.indexes()[location.id]?.error,
    );
  return (
    <div
      class="sessions"
      id={"sessions-" + (props.location?.id ?? props.project?.id ?? "all")}
      {...order.regionProps()}
    >
      <For each={order.ordered()}>
        {(key) => {
          const row = () => rows().find((item) => item.key === key)!;
          const fileDrop = createFileDrop((files) => {
            const { location, thread } = row();
            void app.run(() => Promise.all([
              app.chooseProjectSession(location.id, thread.id),
              app.attachFiles(sessionKey(location.id, thread.id), files),
            ]));
          });
          const selected = () =>
            state.connection?.id === row().location.id &&
            conversationRoot(state.threads, state.selected) ===
              row().thread.id &&
            state.panel === "chat";
          const sort = order.dropProps(
            key,
            t("session {name}", { name: row().thread.title }),
          );
          /* Under a project heading only a worktree or extra folder needs
             naming. In the flat list every row names its project, and a
             worktree row names both. */
          const place = () => {
            const { location } = row();
            if (props.location) return null;
            const icon: IconName = isWorktree(location) ? "branch" : "folder";
            if (props.project)
              return location.workspace === props.project.root
                ? null
                : { icon, text: t(locationName(location)) };
            const project = app
              .projects()
              .find((item) => item.id === projectKey(location));
            const name = project?.label ?? location.label;
            return {
              icon,
              text:
                location.workspace === (project?.root ?? location.workspace)
                  ? name
                  : name + " · " + t(locationName(location)),
            };
          };
          return (
            <div
              class="session-row"
              {...sort}
              ref={(element) => { sort.ref(element); fileDrop.ref(element); }}
              classList={{ selected: selected(), "file-drop-active": fileDrop.dragging() }}
            >
              <button
                aria-description={
                  [
                    row().waiting,
                    !props.project && !props.location
                      ? t("Project {name}", { name: place()?.text ?? "" })
                      : "",
                    isWorktree(row().location)
                      ? t("Worktree: {name}", { name: t(locationName(row().location)) })
                      : "",
                  ]
                    .filter(Boolean)
                    .join(". ") || undefined
                }
                aria-current={selected()}
                onClick={() =>
                  app.run(() =>
                    app.chooseProjectSession(
                      row().location.id,
                      row().thread.id,
                    ),
                  )
                }
              >
                <span
                  class="session-status"
                  classList={{
                    running: !row().waiting && row().running,
                    waiting: !!row().waiting,
                  }}
                  aria-hidden="true"
                />
                <span class="session-label">
                  <span class="session-title">
                    {row().thread.title || t("Untitled session")}
                  </span>
                  <Show when={place()}>
                    {(where) => (
                      <span
                        class="session-location"
                        title={row().location.workspace}
                        aria-hidden="true"
                      >
                        <Icon name={where().icon} size="xs" />
                        {/* The name needs its own box: a bare text node in a
                            flex row cannot take the ellipsis. */}
                        <span>{where().text}</span>
                      </span>
                    )}
                  </Show>
                </span>
                <Show when={row().waiting}>
                  <span class="session-waiting" aria-hidden="true">
                    {row().waiting}
                  </span>
                </Show>
              </button>
              <SessionActions
                archiveOnly
                thread={row().thread}
                connectionId={row().location.id}
              />
            </div>
          );
        }}
      </For>
      <ScheduledSessions rows={rows()} />
      <Show when={!order.ordered().length && !split().groups.length}>
        <p class="sessions-empty" role="status">
          {failures().length
            ? ""
            : loading()
              ? t("Opening…")
              : t("No conversations yet.")}
        </p>
      </Show>
      <For each={failures()}>
        {(location) => (
          <div
            class="project-error"
            role="status"
            title={app.indexes()[location.id]?.error}
          >
            <span>
              {props.project &&
              location.id === primaryLocation(props.project).id
                ? t("Couldn’t open this project.")
                : t("Couldn’t open {name}.", {
                    name: t(locationName(location)),
                  })}
            </span>
            <button
              onClick={() => app.run(() => app.activateProject(location.id))}
            >
              {t("Retry")}
            </button>
          </div>
        )}
      </For>
    </div>
  );
}

type Conversation = ReturnType<
  ReturnType<typeof useApp>["conversations"]
>[number];
function ScheduledSessions(props: { rows: Conversation[] }) {
  const app = useApp();
  const groups = createMemo(() => splitSchedules(props.rows).groups);
  const [expanded, setExpanded] = createSignal(false);
  const [open, setOpen] = createSignal<Record<string, boolean>>({});
  const [limits, setLimits] = createSignal<Record<string, number>>({});
  let selected = "";
  let reveal = true;
  createEffect(() => {
    const key = `${app.state.connection?.id}:${app.state.selected}`;
    const group = groups().find((group) =>
      group.runs.some(
        (row) =>
          row.location.id === app.state.connection?.id &&
          row.thread.id === app.state.selected,
      ),
    );
    if (key !== selected) {
      selected = key;
      reveal = true;
    }
    if (!reveal || !group) return;
    reveal = false;
    setExpanded(true);
    setOpen((values) => ({ ...values, [group.id]: true }));
    const index = group.runs.findIndex(
      (row) => row.thread.id === app.state.selected,
    );
    setLimits((values) => ({
      ...values,
      [group.id]: Math.max(values[group.id] ?? 10, index + 1),
    }));
  });
  return (
    <Show when={groups().length}>
      <section class="scheduled-sessions" aria-label={t("Scheduled conversations")}>
        <button
          class="scheduled-heading"
          aria-expanded={expanded()}
          onClick={() => setExpanded((value) => !value)}
        >
          <Icon name={expanded() ? "down" : "chevron"} size="xs" />
          <Icon name="clock" size="sm" />
          <span>{t("Scheduled")}</span>
          <small>{groups().length}</small>
        </button>
        <Show when={expanded()}>
          <For each={groups().map((group) => group.id)}>
            {(id) => {
              const group = () => groups().find((group) => group.id === id)!;
              return (
                <section
                  class="schedule-group"
                  aria-label={t("Schedule {name}", { name: group().name })}
                >
                  <button
                    class="schedule-group-heading"
                    aria-expanded={!!open()[id]}
                    onClick={() =>
                      setOpen((values) => ({ ...values, [id]: !values[id] }))
                    }
                  >
                    <Icon name={open()[id] ? "down" : "chevron"} size="xs" />
                    <span>{group().name}</span>
                    <small>{group().runs.length}</small>
                  </button>
                  <Show when={open()[id]}>
                    <div class="schedule-runs">
                      <For
                        each={group()
                          .runs.slice(0, limits()[id] ?? 10)
                          .map((row) => row.key)}
                      >
                        {(key) => {
                          const row = () =>
                            group().runs.find((row) => row.key === key)!;
                          const fileDrop = createFileDrop((files) => {
                            const { location, thread } = row();
                            void app.run(() => Promise.all([
                              app.chooseProjectSession(location.id, thread.id),
                              app.attachFiles(sessionKey(location.id, thread.id), files),
                            ]));
                          });
                          const active = () =>
                            app.state.connection?.id === row().location.id &&
                            conversationRoot(
                              app.state.threads,
                              app.state.selected,
                            ) === row().thread.id &&
                            app.state.panel === "chat";
                          const label = () =>
                            new Date(row().thread.createdAt).toLocaleString(
                              [],
                              {
                                month: "short",
                                day: "numeric",
                                hour: "2-digit",
                                minute: "2-digit",
                              },
                            );
                          return (
                            <div
                              ref={fileDrop.ref}
                              class="session-row"
                              classList={{ selected: active(), "file-drop-active": fileDrop.dragging() }}
                            >
                              <button
                                aria-label={t("Open {name} · {time}", {
                                  name: group().name,
                                  time: label(),
                                })}
                                aria-current={active()}
                                title={row().thread.title}
                                onClick={() =>
                                  app.run(() =>
                                    app.chooseProjectSession(
                                      row().location.id,
                                      row().thread.id,
                                    ),
                                  )
                                }
                              >
                                <span
                                  class="session-status"
                                  classList={{
                                    running: !row().waiting && row().running,
                                    waiting: !!row().waiting,
                                  }}
                                  aria-hidden="true"
                                />
                                <span class="session-label">
                                  <span class="session-title">{label()}</span>
                                </span>
                                <Show when={row().waiting}>
                                  <span
                                    class="session-waiting"
                                    aria-hidden="true"
                                  >
                                    {row().waiting}
                                  </span>
                                </Show>
                              </button>
                              <SessionActions
                                archiveOnly
                                thread={row().thread}
                                connectionId={row().location.id}
                              />
                            </div>
                          );
                        }}
                      </For>
                      <Show when={group().runs.length > (limits()[id] ?? 10)}>
                        <button
                          class="schedule-more"
                          onClick={() =>
                            setLimits((values) => ({
                              ...values,
                              [id]: (values[id] ?? 10) + 10,
                            }))
                          }
                        >
                          {t("Show more conversations")}
                        </button>
                      </Show>
                    </div>
                  </Show>
                </section>
              );
            }}
          </For>
        </Show>
      </section>
    </Show>
  );
}
