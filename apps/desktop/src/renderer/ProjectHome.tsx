import { t } from "./i18n";
import { For, Show } from "solid-js";
import { primaryLocation } from "../projects";
import { Mark } from "./brand";
import { useApp } from "./state";
import { Icon } from "./ui";
import "./project-home.css";

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
                    : projects().length
                      ? t("Choose a project")
                      : t("Open a project to get started")}
                </h1>
                <p>
                  {app.currentServer().cloud
                    ? t("Start or open a session from your Haarness account.")
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
                : projects().length
                  ? t("Open another project")
                  : t("Open project")}
          </button>
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
        </Show>
      </div>
    </section>
  );
}
