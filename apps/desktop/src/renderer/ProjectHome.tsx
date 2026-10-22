import { t } from "./i18n";
import { createEffect, createSignal, For, on, Show } from "solid-js";
import type { WorkspaceInfo } from "@haarness/sdk";
import { primaryLocation } from "../projects";
import { Mark } from "./brand";
import { useApp } from "./state";
import { Icon } from "./ui";
import "./project-home.css";

/** How many of another computer's projects show before "Show more". */
const HOST_PROJECTS = 8;

export function ProjectHome() {
  const app = useApp();
  const state = app.state;
  const busy = () => !state.ready || !!state.openingProject;
  const projects = () =>
    app
      .ordered(
        "list:projects",
        app.projects().map((item) => item.id),
      )
      .map((id) => app.projects().find((item) => item.id === id)!);
  const computer = () => app.currentServer().label;
  const host = () => app.currentServer().host;
  // Another computer lists the projects it offers when its home shows, not
  // before: its own catalog, newest first.
  const [catalog, setCatalog] = createSignal<{
    server: string;
    status: "loading" | "ready" | "error";
    items: (WorkspaceInfo & { title: string })[];
    error: string;
  }>();
  const [all, setAll] = createSignal(false);
  async function loadCatalog(server: string) {
    setCatalog({ server, status: "loading", items: [], error: "" });
    try {
      const value = await window.haarness.remoteCatalog(server);
      if (app.currentServer().id !== server) return;
      const names = new Map(value.projects.map((item) => [item.id, item]));
      const items = value.workspaces
        .filter((item) => item.exists && item.kind === "local")
        .sort((a, b) => Date.parse(b.lastOpenedAt) - Date.parse(a.lastOpenedAt))
        .map((item) => {
          const project = names.get(item.projectId);
          return {
            ...item,
            title: project && project.root === item.path ? project.name : item.name,
          };
        });
      setCatalog({ server, status: "ready", items, error: "" });
    } catch (error) {
      if (app.currentServer().id !== server) return;
      setCatalog({
        server,
        status: "error",
        items: [],
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }
  createEffect(
    on(
      () => [app.currentServer().id, !!host(), busy()] as const,
      ([server, remote, waiting]) => {
        setAll(false);
        const known = catalog();
        // Read again after a failure: the computer may be back by now.
        if (remote && !waiting && (known?.server !== server || known.status === "error"))
          void loadCatalog(server);
      },
    ),
  );
  const hostProjects = () =>
    catalog()?.server === app.currentServer().id ? catalog() : undefined;
  const shown = () => {
    const items = hostProjects()?.items ?? [];
    return all() ? items : items.slice(0, HOST_PROJECTS);
  };
  /** Opens a project of the computer, as saved when it was opened before. */
  const openHostProject = (workspace: WorkspaceInfo) => {
    const saved = state.recent.find(
      (item) => item.remoteHost === host() && item.remoteWorkspace === workspace.id,
    );
    return app.run(() =>
      app.connect(
        saved
          ? { recent: saved.id }
          : { remoteHost: host()!, remoteWorkspace: workspace.id },
      ),
    );
  };
  return (
    <section class="project-home" aria-label={t("Project home")} aria-busy={busy()}>
      <div class="project-home-content">
        <div class="start-mark" aria-hidden="true">
          <Mark />
        </div>
        <Show when={!busy()} fallback={<p role="status">{t("Opening project…")}</p>}>
          <Show
            when={state.projectFailure}
            fallback={
              <>
                <h1>
                  {app.currentServer().cloud
                    ? t("Open a Cloud session")
                    : host()
                      ? t("Choose a project on {computer}", { computer: computer() })
                      : projects().length
                        ? t("Choose a project")
                        : t("Open a project to get started")}
                </h1>
                <p>
                  {app.currentServer().cloud
                    ? t("Start or open a session from your Haarness account.")
                    : host()
                      ? t("Prompts, tools and terminals run on {computer}.", { computer: computer() })
                      : app.currentServer().url
                        ? t("Choose the folder where Haarness should work on this server.")
                        : t("Choose the folder where Haarness should work.")}
                </p>
              </>
            }
          >
            {(failure) => (
              <>
                <h1>{t("Couldn’t open project")}</h1>
                <p class="project-home-error" role="alert">
                  {failure().message}
                </p>
                <button
                  onClick={() => app.run(() => app.connect(failure().input))}
                >
                  {t("Retry")}
                </button>
              </>
            )}
          </Show>
          <button
            class="primary project-home-open"
            onClick={() => app.run(() => app.openProject())}
          >
            <Icon name={app.currentServer().cloud ? "cloud" : "folder"} size="md" />
            {app.currentServer().cloud
              ? t("Cloud sessions")
              : state.projectFailure
                ? t("Choose another project")
                : (host() ? hostProjects()?.items.length : projects().length)
                  ? t("Open another project")
                  : t("Open project")}
          </button>
          <Show
            when={host()}
            fallback={
              <Show when={projects().length}>
                <div class="project-home-recent">
                  <h2>{t("Recent projects")}</h2>
                  <ul>
                    <For each={projects()}>
                      {(project) => (
                        <li>
                          <button
                            class="project-home-row"
                            title={primaryLocation(project).cloudSession ? undefined : project.root}
                            onClick={() =>
                              app.run(() =>
                                app.connect({
                                  recent: primaryLocation(project).id,
                                }),
                              )
                            }
                          >
                            <Icon
                              name={primaryLocation(project).cloudSession ? "cloud" : "folder"}
                              size="md"
                            />
                            <span>
                              <strong>{project.label}</strong>
                              <small>
                                {primaryLocation(project).cloudSession
                                  ? t("Cloud session")
                                  : project.root}
                              </small>
                            </span>
                            <Icon name="chevron" size="sm" />
                          </button>
                        </li>
                      )}
                    </For>
                  </ul>
                </div>
              </Show>
            }
          >
            <div class="project-home-recent" aria-busy={hostProjects()?.status === "loading"}>
              <Show when={hostProjects()?.status === "loading" && !state.projectFailure}>
                <p class="project-home-status" role="status">
                  {t("Looking for projects on {computer}…", { computer: computer() })}
                </p>
              </Show>
              {/* A project that could not open already says why above. */}
              <Show when={hostProjects()?.status === "error" && !state.projectFailure}>
                <div class="project-home-status">
                  <p class="project-home-error" role="alert">
                    {hostProjects()!.error}
                  </p>
                  <button onClick={() => void loadCatalog(app.currentServer().id)}>
                    {t("Try again")}
                  </button>
                </div>
              </Show>
              <Show when={shown().length}>
                <h2>{t("Projects on {computer}", { computer: computer() })}</h2>
                <ul>
                  <For each={shown()}>
                    {(workspace) => (
                      <li>
                        <button
                          class="project-home-row"
                          title={workspace.path}
                          onClick={() => openHostProject(workspace)}
                        >
                          <Icon name="folder" size="md" />
                          <span>
                            <strong>{workspace.title}</strong>
                            <small>{workspace.path}</small>
                          </span>
                          <Icon name="chevron" size="sm" />
                        </button>
                      </li>
                    )}
                  </For>
                </ul>
                <Show when={!all() && (hostProjects()?.items.length ?? 0) > HOST_PROJECTS}>
                  <button class="project-home-more" onClick={() => setAll(true)}>
                    {t("Show more")}
                  </button>
                </Show>
              </Show>
            </div>
          </Show>
        </Show>
      </div>
    </section>
  );
}
