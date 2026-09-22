import { createSettingsSearch } from "./settings-search";
import { searchEntries } from "./search";
import { SettingRow as Row } from "./SettingRow";
import { BrowserSettings } from "./BrowserSettings";
import { Notifications } from "./Notifications";
import { t, languages } from "./i18n";
import { InputSettings } from "./InputSettings";
import { openServerConnection } from "./ServerConnect";
import { ManagementPanel } from "./ManagementPanel";
import { ProviderLabel } from "./ProviderIcon";
import { modelName } from "../model-names";
import { Schedules } from "./Schedules";
import { Hooks } from "./Hooks";
import { McpForm } from "./McpForm";
import { AssetForm, type AssetRow } from "./AssetForm";
import { SourceForm } from "./SourceForm";
import { ProviderForm } from "./ProviderForm";
import { RouterConnectionDialog } from "./RouterConnectionDialog";
import { disconnectProvider } from "./ConnectProvider";
import { Select } from "./Select";
import { MenuButton } from "./MenuButton";
import {
  For,
  Show,
  createSignal,
  createEffect,
  onCleanup,
  onMount,
  type JSX,
} from "solid-js";
import type * as T from "@haarness/sdk";
import type { Preferences } from "../shared";
import { themes, themeColors } from "../themes";
import { useApp } from "./state";
import { dismissOnBackdrop } from "./ui";
import { Wordmark } from "./brand";
import { Button, Icon, ask, confirm, stringify, Empty, type Field } from "./ui";
import {
  settingsSections as sections,
  settingsTitles as titles,
  shortcutTitles,
} from "./settings-catalog";
export function Settings() {
  const app = useApp();
  const search = createSettingsSearch(app);
  let content!: HTMLDivElement;
  let dialog!: HTMLDialogElement;
  onMount(() => dialog.showModal());
  onCleanup(() => dialog.close());
  createEffect(() => {
    const target = app.state.settingsTarget;
    app.state.settings;
    app.state.settingsRevision;
    setFilter("");
    if (!target) { queueMicrotask(() => content?.scrollTo({ top: 0 })); return; }
    let marked: HTMLElement | undefined;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const focus = () => {
      const element = content?.querySelector<HTMLElement>(
        `[data-setting="${CSS.escape(target)}"]`,
      );
      if (!element) return;
      marked = element;
      element.classList.add("setting-search-target");
      element.scrollIntoView({ block: "center", behavior: "instant" });
      const control =
        element.querySelector<HTMLElement>(
          "input:not(:disabled), button:not(:disabled), select:not(:disabled), textarea:not(:disabled)",
        ) ?? element;
      if (control === element) element.tabIndex = -1;
      control.focus({ preventScroll: true });
      observer.disconnect();
      timer = setTimeout(
        () => element.classList.remove("setting-search-target"),
        2500,
      );
    };
    const observer = new MutationObserver(focus);
    queueMicrotask(() => {
      if (!disposed && content?.isConnected) {
        observer.observe(content, { childList: true, subtree: true });
        focus();
      }
    });
    onCleanup(() => {
      disposed = true;
      observer.disconnect();
      clearTimeout(timer);
      marked?.classList.remove("setting-search-target");
    });
  });
  const [output, setOutput] = createSignal(""),
    [task, setTask] = createSignal(""),
    [filter, setFilter] = createSignal("");
  const off = window.haarness.onEvent((event) => {
    if (event.kind === "command" && event.id === task())
      setOutput((text) => (text + String(event.data)).slice(-100000));
  });
  onCleanup(off);
  async function cli(args: string[]) {
    const id = crypto.randomUUID();
    setTask(id);
    setOutput("");
    try {
      const value = await window.haarness.command(
        app.state.connection!.id,
        args,
        id,
      );
      setOutput(value.output);
      await app.catalogs();
      return value.value;
    } finally {
      setTask("");
    }
  }
  const connected = () => !!app.state.connection;
  const local = () => connected() && !app.state.connection!.remote;
  return (
    <dialog
      ref={dialog}
      class="settings settings-dialog"
      aria-label={t("Settings")}
      onCancel={(event) => { event.preventDefault(); void app.closeSettings(); }}
      {...dismissOnBackdrop(() => void app.closeSettings())}
    >
        <aside classList={{ "settings-searching": !!filter().trim() }}>
          <h2>
            {t("Settings")}<span class="brand-dot">.</span>
          </h2>
          <input
            class="settings-filter"
            aria-label={t("Filter settings")}
            placeholder={t("Filter…")}
            value={filter()}
            onInput={(event) => setFilter(event.currentTarget.value)}
          />
          <Show when={filter().trim()} fallback={
          <For each={sections}>
            {([group, items]) => {
              const visible = () =>
                items.filter((name) =>
                  `${titles[name]} ${t(titles[name])}`.toLowerCase().includes(filter().toLowerCase()),
                );
              return (
                <Show when={visible().length}>
                  <div class="settings-group">
                    <span class="section-label">{t(group)}</span>
                    <For each={visible()}>
                      {(name) => (
                        <button
                          classList={{ active: app.state.settings === name }}
                          onClick={async () => {
                            if (!(await app.canLeaveSettings())) return;
                            app.set("settingsTarget", null);
                            app.set("settings", name);
                            app.set("settingsRevision", (revision) => revision + 1);
                            setOutput("");
                          }}
                        >
                          {t(titles[name])}
                        </button>
                      )}
                    </For>
                  </div>
                </Show>
              );
            }}
          </For>
          }>
            <nav class="settings-search-results" aria-label={t("Settings search results")}>
              <For each={searchEntries(search(), filter())} fallback={<p class="muted">{t("No matching settings.")}</p>}>
                {(entry) => (
                  <button
                    title={entry.name}
                    onClick={() => void entry.action()}
                  >
                    <span>{entry.name}</span>
                    <small>{entry.detail}</small>
                  </button>
                )}
              </For>
            </nav>
          </Show>
          <span class="small muted">Haarness {app.state.version}</span>
        </aside>
        <div class="settings-main">
          <header>
            <div>
              <h1>{t(titles[app.state.settings ?? "general"])}</h1>
            </div>
            <Button
              icon="close"
              label={t("Close settings")}
              onClick={() => void app.closeSettings()}
            />
          </header>
          <div class="settings-content" ref={content}>
            <Show when={app.state.settings === "general"}>
              <General />
            </Show>
            <Show when={app.state.settings === "browser"}><BrowserSettings /></Show>
            <Show when={app.state.settings === "shortcuts"}>
              <Shortcuts />
            </Show>
            <Show when={app.state.settings === "servers"}>
              <Servers />
            </Show>
            <Show when={app.state.settings === "projects"}>
              <Projects />
            </Show>
            <Show when={app.state.settings === "about"}>
              <About />
            </Show>
            <Show
              when={
                ![
                  "general",
                  "browser",
                  "shortcuts",
                  "servers",
                  "projects",
                  "about",
                ].includes(app.state.settings ?? "")
              }
            >
              <Show
                when={connected()}
                fallback={
                  <Empty
                    title={t("Open project first.")}
                    detail="Runtime settings belong to the selected Haarness installation."
                  />
                }
              >
                <Show
                  when={
                    !local() &&
                    ["agents", "commands", "skills", "extensions"].includes(
                      app.state.settings!,
                    )
                  }
                >
                  <p class="notice-inline">
                    This is a remote project. Runtime catalogs are available
                    below; host configuration and credentials must be managed on
                    that server.
                  </p>
                </Show>
                <Show when={app.state.settings === "providers" && local()}>
                  <Providers />
                </Show>
                <Show
                  when={
                    !local() &&
                    [
                      "providers",
                      "mcp",
                      "hooks",
                      "schedules",
                      "profiles",
                    ].includes(app.state.settings!)
                  }
                >
                  <ManagementPanel resource={app.state.settings!} />
                </Show>
                <Show when={app.state.settings === "models"}>
                  <Models />
                </Show>
                <Show
                  when={["agents", "commands", "skills"].includes(
                    app.state.settings!,
                  )}
                >
                  <Assets kind={app.state.settings!} cli={cli} />
                </Show>
                <Show when={app.state.settings === "tools"}>
                  <Tools />
                </Show>
                <Show when={app.state.settings === "hooks" && local()}>
                  <Hooks />
                </Show>
                <Show when={app.state.settings === "mcp" && local()}>
                  <Mcp cli={cli} />
                </Show>
                <Show when={app.state.settings === "plugins"}>
                  <Plugins />
                </Show>
                <Show when={app.state.settings === "extensions" && local()}>
                  <Extensions cli={cli} />
                </Show>
                <Show when={app.state.settings === "schedules" && local()}>
                  <Schedules cli={cli} />
                </Show>
                <Show when={app.state.settings === "profiles" && local()}>
                  <Profiles cli={cli} />
                </Show>
              </Show>
            </Show>
            <Show
              when={app.state.settings !== "schedules" && (output() || task())}
            >
              <details class="command-output" open>
                <summary>
                  {t("Operation output")}
                  <Show when={task()}>
                    <button onClick={() => window.haarness.cancel(task())}>
                      {t("Cancel operation")}
                    </button>
                  </Show>
                </summary>
                <pre>{output() || "Working…"}</pre>
                <For
                  each={[...output().matchAll(/https?:\/\/[^\s]+/g)].slice(
                    0,
                    4,
                  )}
                >
                  {(match) => (
                    <button
                      onClick={() => window.haarness.openExternal(match[0])}
                    >
                      {t("Open authorization page")}
                    </button>
                  )}
                </For>
              </details>
            </Show>
          </div>
        </div>
      <Notifications />
    </dialog>
  );
}
function General() {
  const app = useApp();
  const systemTheme = matchMedia("(prefers-color-scheme: dark)");
  const [systemDark, setSystemDark] = createSignal(systemTheme.matches);
  const systemChanged = () => setSystemDark(systemTheme.matches);
  systemTheme.addEventListener("change", systemChanged);
  onCleanup(() => systemTheme.removeEventListener("change", systemChanged));
  return (
    <>
      <Row title="Language" description="Language changes the interface only. English is the default.">
        <Select
          aria-label={t("Language")}
          value={app.state.preferences.language}
          options={languages.map(({ code, label }) => ({ value: code, label }))}
          onChange={(language) => app.run(() => app.settings({ language }))}
        />
      </Row>
      <InputSettings />
      <Row
        title="Appearance"
        description="Choose light, dark, or follow your system automatically."
      >
        <Select
          aria-label={t("Appearance")}
          value={app.state.preferences.theme}
          onChange={(value) =>
            app.run(() =>
              app.settings({ theme: value as Preferences["theme"] }),
            )
          }
          options={[
            { value: "light", label: t("Light") },
            { value: "system", label: t("Auto") },
            { value: "dark", label: t("Dark") },
          ]}
        />
      </Row>
      <div class="theme-section" data-setting="Theme">
        <div>
          <h3>{t("Theme")}</h3>
          <p>{t("Every palette works in light, dark, and automatic appearance.")}</p>
        </div>
        <div class="theme-grid" aria-label={t("Color themes")}>
          <For each={themes}>
            {(theme) => {
              const colors = () =>
                themeColors(
                  theme.id,
                  app.state.preferences.theme === "system"
                    ? systemDark()
                      ? "dark"
                      : "light"
                    : app.state.preferences.theme,
                );
              return (
                <button
                  class="theme-option"
                  aria-label={theme.name + " theme"}
                  aria-pressed={app.state.preferences.palette === theme.id}
                  title={theme.description}
                  onClick={() =>
                    app.run(() => app.settings({ palette: theme.id }))
                  }
                >
                  <span
                    class="theme-preview"
                    style={{
                      background: colors().page,
                      "border-color": colors().line,
                    }}
                  >
                    <span
                      class="theme-preview-sidebar"
                      style={{ background: colors().sidebar }}
                    >
                      <i style={{ background: colors().primary }} />
                      <i style={{ background: colors().muted }} />
                      <i style={{ background: colors().muted }} />
                    </span>
                    <span class="theme-preview-chat">
                      <i style={{ background: colors().muted }} />
                      <i style={{ background: colors().muted }} />
                      <span style={{ background: colors().surface }}>
                        <b style={{ background: colors().primary }} />
                      </span>
                    </span>
                  </span>
                  <span class="theme-option-label">
                    {theme.name}
                    <Show when={app.state.preferences.palette === theme.id}>
                      <Icon name="check" size="sm" />
                    </Show>
                  </span>
                </button>
              );
            }}
          </For>
        </div>
      </div>
      <Row
        title="Conversation text size"
        description="Applies to messages and the composer. Use View → Zoom to scale the whole window."
      >
        <input
          aria-label={t("Text size")}
          type="range"
          min={11}
          max={24}
          value={app.state.preferences.fontSize}
          onInput={(event) =>
            app.run(() =>
              app.settings({ fontSize: Number(event.currentTarget.value) }),
            )
          }
        />
        <span>{app.state.preferences.fontSize}px</span>
      </Row>
      <Row title="Terminal text size">
        <input
          aria-label={t("Terminal text size")}
          type="number"
          min={10}
          max={24}
          value={app.state.preferences.terminalFontSize}
          onChange={(event) =>
            app.run(() =>
              app.settings({
                terminalFontSize: Number(event.currentTarget.value),
              }),
            )
          }
        />
      </Row>
      <For
        each={
          [
            [
              "notifications",
              "Task notifications",
              "Enable desktop notifications. Choose which alerts appear below.",
            ],
            [
              "compactTools",
              "Compact tool activity",
              "Keep commands and tool output folded until you open them.",
            ],
            [
              "showReasoning",
              "Show reasoning",
              "Display the reasoning supplied by your selected model.",
            ],
            [
              "editorWrap",
              "Wrap editor lines",
              "Keep long lines within the editor width.",
            ],
          ] as const
        }
      >
        {([key, title, description]) => (
          <Row title={title} description={description}>
            <input
              type="checkbox"
              role="switch"
              aria-label={t(title)}
              checked={!!app.state.preferences[key]}
              onChange={(event) =>
                app.run(() =>
                  app.settings({ [key]: event.currentTarget.checked }),
                )
              }
            />
          </Row>
        )}
      </For>
      <Row title="Turn completion notifications" description="Choose when completed or failed tasks notify you.">
        <Select aria-label={t("Turn completion notifications")} value={app.state.preferences.completionNotifications}
          options={[{ value: "background", label: "Only when unfocused" }, { value: "always", label: "Always" }, { value: "never", label: "Never" }]}
          onChange={(value) => app.run(() => app.settings({ completionNotifications: value as Preferences["completionNotifications"] }))} />
      </Row>
      <For each={[["permissionNotifications", "Permission notifications", "Notify when approval is needed while the app is in the background."], ["questionNotifications", "Question notifications", "Notify when input is needed while the app is in the background."]] as const}>
        {([key, title, description]) => <Row title={title} description={description}><input type="checkbox" role="switch" aria-label={title} checked={app.state.preferences[key]} onChange={(event) => app.run(() => app.settings({ [key]: event.currentTarget.checked }))} /></Row>}
      </For>
      <Row title="Send message with">
        <Select
          aria-label={t("Send message with")}
          value={app.state.preferences.sendKey}
          onChange={(value) =>
            app.run(() =>
              app.settings({ sendKey: value as Preferences["sendKey"] }),
            )
          }
          options={[
            { value: "enter", label: t("Enter") },
            { value: "mod-enter", label: "Command / Control + Enter" },
          ]}
        />
      </Row>
      <p class="muted">
        Drafts, appearance and saved projects are saved on this computer.
        Conversations and tasks are saved separately for each project folder or
        worktree.
      </p>
    </>
  );
}
function Shortcuts() {
  const app = useApp();
  return (
    <>
      <p class="muted">
        Use Mod for Command on macOS and Control on Windows/Linux. The editor
        also supports standard search, selection, undo, and save shortcuts.
      </p>
      <For each={Object.entries(app.state.preferences.shortcuts)}>
        {([name, value]) => (
          <Row title={shortcutTitles[name] ?? name} target={"shortcut:" + name}>
            <input
              aria-label={t("Shortcut {name}", { name })}
              value={value}
              onChange={(event) =>
                app.run(() =>
                  app.settings({
                    shortcuts: {
                      ...app.state.preferences.shortcuts,
                      [name]: event.currentTarget.value,
                    },
                  }),
                )
              }
            />
          </Row>
        )}
      </For>
      <Row title="New line">
        <kbd>{t("Shift Enter")}</kbd>
      </Row>
      <Row title="Save file">
        <kbd>{t("Mod S")}</kbd>
      </Row>
      <Row title="Find in file">
        <kbd>{t("Mod F")}</kbd>
      </Row>
      <Row title="Close dialog">
        <kbd>{t("Escape")}</kbd>
      </Row>
    </>
  );
}
function Servers() {
  const app = useApp();
  return (
    <>
      <button data-setting="Add remote server" class="primary" onClick={openServerConnection}>
        {t("Add remote server")}
      </button>
      <p class="muted">
        {t("Select a server to see its projects and conversations.")}
      </p>
      <For each={app.servers()}>
        {(item) => (
          <Row
            title={item.label}
            target={"server:" + item.id}
            description={item.url ?? "Local projects on this computer"}
          >
            <button
              disabled={
                app.state.activeServer === item.id && !app.state.openingProject
              }
              onClick={() => app.run(() => app.switchServer(item.id))}
            >
              {app.state.activeServer === item.id ? "Selected" : "Switch"}
            </button>
            <Show when={item.url}>
              <button
                onClick={() =>
                  app.run(async () => {
                    const input = await ask("Rename server", [
                      {
                        name: "label",
                        label: t("Name"),
                        value: item.label,
                        required: true,
                      },
                    ]);
                    if (input)
                      app.set(
                        "servers",
                        await window.haarness.server({
                          id: item.id,
                          action: "rename",
                          label: input.label,
                        }),
                      );
                  })
                }
              >
                {t("Rename")}
              </button>
              <button
                onClick={() =>
                  app.run(async () => {
                    if (
                      await confirm(
                        "Remove this server?",
                        "This removes the saved connection and its projects from Desktop. Conversation history and running tasks on the server are preserved.",
                      )
                    )
                      await app.removeServer(item.id);
                  })
                }
              >
                {t("Remove")}
              </button>
            </Show>
          </Row>
        )}
      </For>
    </>
  );
}
function Projects() {
  const app = useApp();
  return (
    <>
      <Show when={app.state.connection}>
        {(connection) => (
          <section class="settings-current-project" aria-label={t("Current working folder")} data-setting="Current working folder">
            <div class="settings-current-project-icon"><Icon name="folder" size="lg" /></div>
            <div class="settings-current-project-details">
              <span class="small muted">{t("Current working folder")}</span>
              <h3>{connection().workspace.split(/[\\/]/).filter(Boolean).at(-1) || connection().workspace}</h3>
              <p class="settings-current-project-path">{connection().workspace}</p>
              <span class="small muted">{app.currentServer().label}</span>
              <Show when={!connection().remote && !connection().trust}>
                <p class="small muted">{t("Project configuration disabled")}</p>
              </Show>
            </div>
            <Button icon="refresh" label={t("Refresh project")} onClick={() => app.run(app.hydrate)} />
          </section>
        )}
      </Show>
      <div class="setting-block">
        <h3>{app.currentServer().label}</h3>
        <p>
          Manage projects on this server. Switch servers to see another server’s
          projects.
        </p>
        <button onClick={() => app.openSettings("servers")}>
          {t("Switch server")}
        </button>
        <button
          class="primary"
          onClick={() => app.run(() => app.openProject())}
        >
          {t("Open project")}
        </button>
      </div>
      <Show when={!app.projects().length}>
        <Empty
          title={t("No projects yet")}
          detail="Open a project on this server to get started."
        />
      </Show>
      <For each={app.projects()}>
        {(item) => (
          <Row title={item.label} target={"project:" + item.id} description={item.root}>
            <button onClick={() => app.run(() => app.selectProject(item.id))}>
              {t("Open")}
            </button>
            <button
              onClick={() =>
                app.run(async () => {
                  if (
                    await confirm(
                      t("Forget this project?"),
                      t(
                        "Conversation history, running tasks and the saved server connection are preserved.",
                      ),
                    )
                  )
                    await app.forgetProject(item.id);
                })
              }
            >
              {t("Forget")}
            </button>
          </Row>
        )}
      </For>

    </>
  );
}
type Cli = (args: string[]) => Promise<unknown>;
function Providers() {
  const app = useApp();
  const local = () => !app.state.connection?.remote;
  const [adding, setAdding] = createSignal(false);
  const [addingRouter, setAddingRouter] = createSignal(false);
  const connected = () =>
    app.state.providers.filter((provider) => provider.authenticated);
  const rows = () =>
    [...app.state.providers].sort(
      (a, b) => Number(b.authenticated) - Number(a.authenticated),
    );
  const summary = (provider: (typeof app.state.providers)[number]) => {
    if (!provider.authenticated) return t("Not connected");
    const count = provider.models.length;
    return [
      t("Connected"),
      count === 1 ? t("1 model") : count ? t("{count} models").replace("{count}", String(count)) : "",
    ]
      .filter(Boolean)
      .join(" · ");
  };
  return (
    <>
      <div class="setting-block providers-hero">
        <header>
          <div>
            <h3>
              {connected().length
                ? t("Connected providers")
                : t("No provider connected yet")}
            </h3>
            <p class="muted">
              {connected().length
                ? t("Sign in to another provider or add a key. Keys stay on this Haarness server.")
                : t("Sign in with a ChatGPT subscription, or paste an API key from OpenAI, Anthropic, Google and more.")}
            </p>
          </div>
          <button
            class="primary"
            data-setting="Connect a provider"
            onClick={() => app.openConnect({ onboarding: !connected().length })}
          >
            {t("Connect a provider")}
          </button>
        </header>
        <div class="toolbar">
          <button onClick={() => app.run(() => app.catalogs(true))}>
            {t("Refresh models")}
          </button>
          <Show when={local()}>
            <button onClick={() => setAdding(true)}>{t("Add custom provider")}</button>
            <button onClick={() => setAddingRouter(true)}>
              {t("Connect Haarness Router")}
            </button>
          </Show>
        </div>
      </div>
      <Show when={adding()}>
        <ProviderForm close={() => setAdding(false)} />
      </Show>
      <Show when={addingRouter()}>
        <RouterConnectionDialog close={() => setAddingRouter(false)} />
      </Show>
      <For each={rows()}>
        {(provider) => (
          <div class="setting-block">
            <header>
              <div>
                <h3 data-setting={"provider:" + provider.id}>
                  <ProviderLabel provider={provider.id} size="lg">
                    {provider.name}
                  </ProviderLabel>
                </h3>
                <span classList={{ added: provider.authenticated }}>
                  {summary(provider)}
                </span>
              </div>
              <div class="toolbar">
                <button
                  classList={{ primary: !provider.authenticated }}
                  onClick={() => app.openConnect({ provider: provider.id, onboarding: false })}
                >
                  {provider.authenticated ? t("Reconnect") : t("Connect")}
                </button>
                <Show when={provider.authenticated}>
                  <button
                    onClick={() =>
                      app.run(() => disconnectProvider(app, provider))
                    }
                  >
                    {t("Disconnect")}
                  </button>
                </Show>
              </div>
            </header>
            <details>
              <summary>{t("Available models")}</summary>
              <For each={provider.models}>
                {(model) => (
                  <p>
                    <strong>
                      {modelName(provider.id, model.id, app.state.providers)}
                    </strong>{" "}
                    <code>{model.id}</code> ·{" "}
                    {model.contextWindow?.toLocaleString() ?? "Unknown"} context
                  </p>
                )}
              </For>
            </details>
          </div>
        )}
      </For>
    </>
  );
}
function Models() {
  const app = useApp();
  const [filter, setFilter] = createSignal("");
  return (
    <>
      <input
        placeholder={t("Search models…")}
        aria-label={t("Search models")}
        value={filter()}
        onInput={(event) => setFilter(event.currentTarget.value)}
      />
      <table>
        <thead>
          <tr>
            <th>{t("Model")}</th>
            <th>{t("Provider")}</th>
            <th>{t("Context")}</th>
            <th>{t("Capabilities")}</th>
          </tr>
        </thead>
        <tbody>
          <For
            each={app.state.providers
              .flatMap((provider) =>
                provider.models.map((model) => ({ provider, model })),
              )
              .filter((item) =>
                (
                  modelName(
                    item.provider.id,
                    item.model.id,
                    app.state.providers,
                  ) +
                  " " +
                  item.model.id +
                  " " +
                  item.provider.name
                )
                  .toLowerCase()
                  .includes(filter().toLowerCase()),
              )}
          >
            {({ provider, model }) => (
              <tr data-setting={"model:" + provider.id + ":" + model.id}>
                <td>
                  {modelName(provider.id, model.id, app.state.providers)}
                  <small>{model.id}</small>
                </td>
                <td>
                  <ProviderLabel provider={provider.id}>
                    {provider.name}
                  </ProviderLabel>
                </td>
                <td>{model.contextWindow?.toLocaleString() ?? "—"}</td>
                <td>
                  {[
                    model.tools ? "Tools" : "",
                    model.images ? "Images" : "",
                    model.video ? "Video" : "",
                    model.reasoning ? "Reasoning" : "",
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                </td>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </>
  );
}
function Assets(props: { kind: string; cli: Cli }) {
  const app = useApp();
  const [rows, setRows] = createSignal<AssetRow[]>([]);
  const kind = () =>
    props.kind === "agents"
      ? "agent"
      : props.kind === "skills"
        ? "skill"
        : "command";
  async function refresh() {
    setRows(
      await app.native({
        action: "asset",
        kind: kind(),
        input: { action: "list" },
      }),
    );
  }
  createEffect(() => {
    props.kind;
    app.state.catalogVersion;
    void app.run(refresh);
  });
  const [editing, setEditing] = createSignal<{
    prior: Record<string, unknown>;
    editing: boolean;
  }>();
  async function edit(item?: AssetRow) {
    let prior: Record<string, unknown> = {};
    if (item)
      prior = await app.native({
        action: "asset",
        kind: kind(),
        input: {
          action: "get",
          name: item.name,
          ...(item.scope ? { scope: item.scope } : {}),
        },
      });
    setEditing({ prior, editing: !!item });
  }
  return (
    <>
      <p class="muted">
        Edit your agents, commands, and skills. Changes to built-in or plugin
        definitions are saved as your own version.
      </p>
      <button class="primary" onClick={() => app.run(() => edit())}>
        Create {kind()}
      </button>
      <Show when={editing()} keyed>
        {(current) => (
          <AssetForm
            kind={kind()}
            prior={current.prior}
            editing={current.editing}
            close={() => setEditing(undefined)}
            saved={() => {
              void app.run(async () => {
                await app.catalogs();
                await refresh();
              });
            }}
          />
        )}
      </Show>
      <For each={rows()}>
        {(item) => (
          <Row
            title={item.name}
            description={[
              item.description,
              item.scope === "local"
                ? "This project"
                : item.scope === "global"
                  ? "All projects"
                  : "Built-in / plugin",
            ]
              .filter(Boolean)
              .join(" · ")}
            target={props.kind + ":" + item.name}
          >
            <button onClick={() => app.run(() => edit(item))}>{t("Edit")}</button>
            <Show when={item.managed}>
              <button
                onClick={() =>
                  app.run(async () => {
                    if (
                      await confirm(
                        (item.customized ? "Reset " : "Remove ") + item.name,
                        "Remove this custom " +
                          kind() +
                          " from " +
                          (item.scope === "local"
                            ? "this project"
                            : "all projects") +
                          "?",
                      )
                    ) {
                      await app.native({
                        action: "asset",
                        kind: kind(),
                        input: {
                          action: "delete",
                          name: item.name,
                          scope: item.scope,
                        },
                      });
                      await refresh();
                    }
                  })
                }
              >
                {item.customized ? "Reset to default" : "Remove"}
              </button>
            </Show>
          </Row>
        )}
      </For>
    </>
  );
}
function Tools() {
  const app = useApp();
  const [catalog, setCatalog] = createSignal<T.ToolCatalogResponse>(),
    [filter, setFilter] = createSignal("");
  async function refresh() {
    setCatalog(
      await app.api(
        "toolCatalog",
        app.state.selected ? { threadId: app.state.selected } : {},
      ),
    );
  }
  createEffect(() => {
    app.state.catalogVersion;
    void app.run(refresh);
  });
  return (
    <>
      <Row
        title={t("Tool loading")}
        description="Control how optional tool schemas enter model context."
      >
        <Select
          aria-label={t("Tool loading")}
          value={catalog()?.lazyMode ?? "auto"}
          onChange={(value) =>
            app.run(async () =>
              setCatalog(await app.api("updateLazyTools", { mode: value })),
            )
          }
          options={[
            { value: "auto", label: "Automatic" },
            { value: "always", label: "Always lazy" },
            { value: "off", label: "All enabled tools" },
          ]}
        />
      </Row>
      <p class="muted">
        {catalog()?.estimatedAdvertisedTokens.toLocaleString()} advertised
        tokens · {catalog()?.estimatedSavedTokens.toLocaleString()} saved
      </p>
      <input
        aria-label={t("Search tools")}
        placeholder={t("Find a tool…")}
        onInput={(event) => setFilter(event.currentTarget.value)}
      />
      <For
        each={catalog()?.tools.filter((tool) =>
          tool.id.toLowerCase().includes(filter().toLowerCase()),
        )}
      >
        {(tool) => (
          <Row
            title={tool.id}
            description={tool.description}
            target={"tool:" + tool.id}
          >
            <span class="badge">{tool.source}</span>
            <input
              type="checkbox"
              role="switch"
              aria-label={t("Enable {name}", { name: tool.id })}
              checked={tool.enabled}
              onChange={(event) =>
                app.run(async () =>
                  setCatalog(
                    await app.api("updateTool", tool.id, {
                      enabled: event.currentTarget.checked,
                    }),
                  ),
                )
              }
            />
          </Row>
        )}
      </For>
    </>
  );
}
function Mcp(props: { cli: Cli }) {
  const app = useApp();
  const [rows, setRows] = createSignal<T.McpServerInfo[]>([]);
  async function refresh() {
    setRows(await app.api("mcpServers"));
  }
  createEffect(() => {
    app.state.catalogVersion;
    void app.run(refresh);
  });
  const [adding, setAdding] = createSignal(false);
  return (
    <>
      <Show when={!app.state.connection?.remote}>
        <button class="primary" onClick={() => setAdding(true)}>
          {t("Add MCP server")}
        </button>
      </Show>
      <Show when={adding()}>
        <McpForm
          close={() => setAdding(false)}
          save={async (input) => {
            await app.native({ action: "asset", kind: "mcp", input });
            // Asset events refresh the live registry once the daemon has reloaded.
            void app.run(async () => {
              await app.catalogs();
              await refresh();
            });
          }}
        />
      </Show>
      <For each={rows()}>
        {(server) => (
          <div class="setting-block">
            <header>
              <h3 data-setting={"mcp:" + server.name}>{server.name}</h3>
              <span class="badge">{server.status}</span>
            </header>
            <div class="toolbar">
              <button
                onClick={() =>
                  app.run(async () => {
                    await app.api("reconnectMcp", server.name);
                    await refresh();
                  })
                }
              >
                {t("Reconnect")}
              </button>
              <button
                onClick={() =>
                  app.run(async () => {
                    await app.api("disconnectMcp", server.name);
                    await refresh();
                  })
                }
              >
                {t("Disconnect")}
              </button>
              <Show when={!app.state.connection?.remote}>
                <button
                  onClick={() =>
                    app.run(() => props.cli(["mcp", "auth", server.name]))
                  }
                >
                  {t("Authorize")}
                </button>
                <MenuButton
                  text="Manage"
                  label={t("Manage {name}", { name: server.name })}
                  items={[
                    { label: "Enable", command: "enable" },
                    { label: "Disable", command: "disable" },
                    { label: "Remove", command: "remove" },
                    { label: "Sign out", command: "logout" },
                  ].map((item) => ({
                    label: item.label,
                    run: () =>
                      void app.run(async () => {
                        const input = await ask(
                          item.label + " " + server.name,
                          [
                            {
                              name: "scope",
                              label: "Scope",
                              type: "select",
                              options: ["Global", "Project"],
                            },
                          ],
                          { submit: item.label },
                        );
                        if (!input) return;
                        await props.cli([
                          "mcp",
                          item.command,
                          server.name,
                          ...(input.scope === "Project" ? ["--project"] : []),
                        ]);
                        await refresh();
                      }),
                  }))}
                />
              </Show>
            </div>
          </div>
        )}
      </For>
      <Show when={!rows().length}>
        <Empty
          title={t("Connect your tools.")}
          detail="MCP servers bring external tools, resources and prompts into Haarness."
        />
      </Show>
    </>
  );
}
function Plugins() {
  const app = useApp();
  const [rows, setRows] = createSignal<T.PluginInfo[]>([]);
  async function refresh() {
    setRows(await app.api("plugins"));
  }
  createEffect(() => {
    app.state.catalogVersion;
    void app.run(refresh);
  });
  const [installing, setInstalling] = createSignal<"plugin" | "marketplace">();
  async function action(action: string, target: string) {
    await app.api("pluginAction", { action, target });
    await refresh();
    await app.catalogs();
  }
  return (
    <>
      <p class="muted">
        Install compatible plugin bundles with skills, commands, agents, MCP
        servers and extensions.
      </p>
      <div class="toolbar">
        <button class="primary" onClick={() => setInstalling("plugin")}>
          {t("Install plugin")}
        </button>
        <button onClick={() => setInstalling("marketplace")}>
          {t("Marketplaces")}
        </button>
      </div>
      <Show when={installing()} keyed>
        {(kind) => (
          <SourceForm
            kind={kind}
            close={() => setInstalling(undefined)}
            save={(target) =>
              action(kind === "plugin" ? "install" : "marketplace", target)
            }
          />
        )}
      </Show>
      <For each={rows()}>
        {(plugin) => (
          <Row
            title={plugin.name}
            target={"plugin:" + plugin.name}
            description={plugin.description || plugin.source}
          >
            <Show when={plugin.managed}>
              <input
                type="checkbox"
                role="switch"
                aria-label={t("Enable plugin {name}", { name: plugin.name })}
                checked={plugin.enabled}
                onChange={(event) =>
                  app.run(() =>
                    action(
                      event.currentTarget.checked ? "enable" : "disable",
                      plugin.name,
                    ),
                  )
                }
              />
              <Show when={plugin.source === "haarness"}>
                <button
                  onClick={() => app.run(() => action("update", plugin.name))}
                >
                  {t("Update")}
                </button>
              </Show>
              <button
                onClick={() =>
                  app.run(async () => {
                    if (await confirm("Remove plugin?", plugin.name))
                      await action("remove", plugin.name);
                  })
                }
              >
                {t("Remove")}
              </button>
            </Show>
          </Row>
        )}
      </For>
    </>
  );
}
interface ExtensionConfig {
  enabled: boolean;
  timeout_ms: number;
  entries: {
    name?: string;
    source: string;
    enabled: boolean;
    options: unknown;
  }[];
}
function Extensions(props: { cli: Cli }) {
  const app = useApp();
  const [scope, setScope] = createSignal(app.state.settingsTarget?.startsWith("extension:project:") ? "Project" : "Global");
  createEffect(() => {
    const target = app.state.settingsTarget;
    if (target?.startsWith("extension:")) setScope(target.startsWith("extension:project:") ? "Project" : "Global");
  });
  const flags = () => (scope() === "Project" ? ["--project"] : []);
  const [config, setConfig] = createSignal<ExtensionConfig>();
  async function refresh() {
    setConfig(
      await app.native({
        action: "extensions",
        project: scope() === "Project",
      }),
    );
  }
  createEffect(() => {
    void app.run(refresh);
  });
  const [installing, setInstalling] = createSignal(false);
  async function install(source: string) {
    await props.cli([
      "extension",
      ...flags(),
      "install",
      source,
      "--options",
      JSON.stringify(
        config()?.entries.find((entry) => entry.source === source)?.options ??
          {},
      ),
    ]);
    await refresh();
  }
  return (
    <>
      <Show when={installing()}>
        <SourceForm
          kind="extension"
          close={() => setInstalling(false)}
          save={install}
        />
      </Show>
      <div class="toolbar" data-setting="Extension scope">
        <label>
          Scope{" "}
          <Select
            aria-label={t("Extension scope")}
            value={scope()}
            onChange={(value) => setScope(value)}
            options={[
              { value: "Global", label: "Global" },
              { value: "Project", label: t("Project") },
            ]}
          />
        </label>
      </div>
      <Row
        title={t("Enable extensions")}
        description={
          scope() === "Project"
            ? "Extensions configured for this project."
            : "Extensions available across your projects."
        }
      >
        <input
          role="switch"
          type="checkbox"
          aria-label={t("Enable extensions")}
          checked={config()?.enabled ?? false}
          onChange={(event) =>
            app.run(async () => {
              await props.cli([
                "extension",
                ...flags(),
                event.currentTarget.checked ? "enable" : "disable",
              ]);
              await refresh();
            })
          }
        />
      </Row>
      <div class="toolbar">
        <button data-setting="Install extension" class="primary" onClick={() => setInstalling(true)}>
          {t("Install extension")}
        </button>
      </div>
      <For each={config()?.entries}>
        {(entry, index) => (
          <div class="setting-block" data-setting={"extension:" + scope().toLowerCase() + ":" + entry.source}>
            <header>
              <h3>{entry.name ?? "Extension " + (index() + 1)}</h3>
              <input
                type="checkbox"
                role="switch"
                aria-label={t("Enable extension {name}", { name: entry.name ?? index() + 1 })}
                checked={entry.enabled}
                onChange={(event) =>
                  app.run(async () => {
                    await props.cli([
                      "extension",
                      ...flags(),
                      event.currentTarget.checked ? "enable" : "disable",
                      String(index() + 1),
                    ]);
                    await refresh();
                  })
                }
              />
            </header>
            <p class="path">{entry.source}</p>
            <div class="toolbar">
              <button
                onClick={() =>
                  app.run(async () => {
                    if (
                      await confirm(
                        "Remove extension?",
                        `Unregister ${entry.name ?? entry.source}. Source files are preserved.`,
                      )
                    ) {
                      await props.cli([
                        "extension",
                        ...flags(),
                        "remove",
                        String(index() + 1),
                      ]);
                      await refresh();
                    }
                  })
                }
              >
                {t("Remove")}
              </button>
            </div>
          </div>
        )}
      </For>
      <p class="muted">{t("Changes take effect after the current task finishes.")}</p>
    </>
  );
}
function Profiles(props: { cli: Cli }) {
  const app = useApp();
  return (
    <>
      <p class="muted">
        Portable profiles contain configuration, skills, agents, commands, and
        other supported assets. They exclude conversation databases and
        credential stores.
      </p>
      <Row title="Save a profile">
        <button
          onClick={() =>
            app.run(async () => {
              const input = await ask("Save profile", [
                { name: "name", label: t("Name"), required: true },
                {
                  name: "scope",
                  label: "Scope",
                  type: "select",
                  options: ["both", "global", "local"],
                },
              ]);
              if (input)
                await props.cli([
                  "profile",
                  "save",
                  input.name,
                  "--scope",
                  input.scope,
                  "--json",
                ]);
            })
          }
        >
          {t("Save profile")}
        </button>
      </Row>
      <Row
        title="Restore configuration"
        description="Preview the changes before confirming restoration."
      >
        <button
          onClick={() =>
            app.run(async () => {
              const path = await window.haarness.pick("file");
              if (!path) return;
              const preview = await props.cli([
                "profile",
                "restore",
                path,
                "--dry-run",
                "--json",
              ]);
              if (await confirm("Restore profile?", stringify(preview)))
                await props.cli(["profile", "restore", path, "--json"]);
            })
          }
        >
          {t("Restore profile")}
        </button>
      </Row>
      <Row title="Profile catalog">
        <MenuButton
          text="Manage catalog"
          label={t("Manage catalog")}
          items={[
            { label: "List profiles", command: "cloud-list" },
            { label: "Upload profile", command: "cloud-push" },
            { label: "View profile", command: "cloud-get" },
            { label: "Download profile", command: "cloud-download" },
            { label: "Restore profile", command: "cloud-restore" },
            { label: "Delete profile", command: "cloud-delete" },
          ].map((item) => ({
            label: item.label,
            run: () =>
              void app.run(async () => {
                const input = await ask(
                  item.label,
                  [
                    { name: "url", label: "HTTPS URL", required: true },
                    ...(item.command === "cloud-list"
                      ? []
                      : [
                          {
                            name: "target",
                            label:
                              item.command === "cloud-push"
                                ? "Local archive path"
                                : "Profile ID",
                            required: true,
                          },
                        ]),
                    ...(item.command === "cloud-download"
                      ? [
                          {
                            name: "output",
                            label: "Download output path",
                            required: true,
                          },
                        ]
                      : []),
                  ],
                  { submit: item.label },
                );
                if (!input) return;
                const args = [
                  "profile",
                  item.command,
                  ...(input.target ? [input.target] : []),
                  "--url",
                  input.url,
                  "--json",
                ];
                if (input.output) args.push("--output", input.output);
                await props.cli(args);
              }),
          }))}
        />
      </Row>
    </>
  );
}
function About() {
  const app = useApp();
  const [update, setUpdate] = createSignal<{
    status?: string;
    percent?: number;
    version?: string;
    message?: string;
  }>({});
  const off = window.haarness.onEvent((event) => {
    if (event.kind === "update")
      setUpdate(event.data as ReturnType<typeof update>);
  });
  onCleanup(off);
  return (
    <>
      <Wordmark class="about-mark" />
      <p>Haarness {app.state.version}</p>
      <p class="muted">{t("A place for your projects and conversations.")}</p>
      <Row title="Updates">
        <button
          onClick={() =>
            app.run(async () => {
              const result = (await window.haarness.update("check")) as {
                available: boolean;
                info?: { version: string };
              };
              setUpdate({
                status: result.available ? "available" : "current",
                version: result.info?.version,
              });
            })
          }
        >
          {t("Check for updates")}
        </button>
        <Show when={update().status === "available"}>
          <button
            onClick={() => app.run(() => window.haarness.update("download"))}
          >
            Download {update().version}
          </button>
        </Show>
        <Show when={update().status === "ready"}>
          <button
            class="primary"
            onClick={() =>
              app.run(async () => {
                if (
                  await confirm(
                    "Install update?",
                    "Haarness will restart. Save your file edits before continuing.",
                  )
                )
                  await window.haarness.update("install");
              })
            }
          >
            {t("Restart & install")}
          </button>
        </Show>
        <Show when={update().status === "downloading"}>
          <span>Downloading {Math.round(update().percent ?? 0)}%</span>
        </Show>
        <Show when={update().status === "current"}>
          <span>{t("You are up to date.")}</span>
        </Show>
        <Show when={update().message}>
          <span class="danger">{update().message}</span>
        </Show>
      </Row>
      <Show when={app.state.connection && !app.state.connection.remote}>
        <Row
          title="Reopen project"
          description="Reload the current project to bring conversations and files up to date."
        >
          <button onClick={() => app.run(app.hydrate)}>Refresh</button>
        </Row>
      </Show>
    </>
  );
}
