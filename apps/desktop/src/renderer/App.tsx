import { BrowserPanel } from "./BrowserPanel";
import { Notifications } from "./Notifications";
import { t } from "./i18n";
import { ServerConnect, openServerConnection } from "./ServerConnect";
import { RemoteProjectPicker } from "./RemoteProjectPicker";
import { ConnectProvider } from "./ConnectProvider";
import {
  For,
  Show,
  createEffect,
  createMemo,
  lazy,
  Suspense,
  createSignal,
  onMount,
  onCleanup,
} from "solid-js";
import { useApp } from "./state";
import {
  isWorktree,
  locationName,
  primaryLocation,
  sidebarGroups,
} from "../projects";
import { Button, Icon, Forms, ask, type IconName } from "./ui";
import { Session } from "./Session";
import { ProjectHome } from "./ProjectHome";
import { ChatLoading } from "./ChatLoading";
import { ConversationContext } from "./ConversationContext";
import { ProjectRows, ProjectSection, SessionList } from "./ProjectGroup";
import { SessionActions, renameSession } from "./SessionActions";
import { ResizeHandle } from "./ResizeHandle";
import { createOrdering, createOrderDropTarget } from "./Sortable";
import { MenuButton } from "./MenuButton";
import { UniversalSearch } from "./UniversalSearch";
import { Wordmark } from "./brand";
import { shortcutLabel, type SearchEntry } from "./search";
import { createWorktree, resetWorktree, removeWorktree } from "./worktrees";
type Dock = "files" | "diff" | "context" | "browser";

/** Workbench panels and their launcher copy. */
const panelChoices: {
  name: string;
  icon: IconName;
  detail: string;
  shortcut: string;
}[] = [
  { name: "Browser", icon: "web", detail: "Browse and test websites with Haarness", shortcut: "browser" },
  {
    name: "Files",
    icon: "folder",
    detail: "Browse and edit project files",
    shortcut: "files",
  },
  {
    name: "Review",
    icon: "diff",
    detail: "Inspect changes and compare diffs",
    shortcut: "diff",
  },
  {
    name: "Terminal",
    icon: "terminal",
    detail: "Run commands in this project",
    shortcut: "terminal",
  },
  {
    name: "Context",
    icon: "chart",
    detail: "Inspect conversation tokens, cache and context",
    shortcut: "context",
  },
];
const dockNames: Record<Dock, string> = {
  browser: "Browser",
  files: "Files",
  diff: "Review",
  context: "Context",
};
const dockIcons: Record<Dock, IconName> = {
  browser: "web",
  files: "folder",
  diff: "diff",
  context: "chart",
};
const Explorer = lazy(() =>
  import("./Explorer").then((module) => ({ default: module.Explorer })),
);
import { Review } from "./Review";
const TerminalPanel = lazy(() =>
  import("./Terminal").then((module) => ({ default: module.TerminalPanel })),
);
const MemoryDialog = lazy(() => import("./MemoryDialog").then((module) => ({default: module.MemoryDialog})));
const Settings = lazy(() =>
  import("./Settings").then((module) => ({ default: module.Settings })),
);
export function App() {
  const app = useApp();
  const { state, set } = app;
  onMount(() => {
    const pointer = () => document.documentElement.setAttribute("data-pointer-focus", "");
    const keyboard = (event: KeyboardEvent) => {
      if (!["Meta", "Control", "Alt", "Shift"].includes(event.key))
        document.documentElement.removeAttribute("data-pointer-focus");
    };
    document.addEventListener("pointerdown", pointer, true);
    document.addEventListener("keydown", keyboard, true);
    onCleanup(() => {
      document.removeEventListener("pointerdown", pointer, true);
      document.removeEventListener("keydown", keyboard, true);
      document.documentElement.removeAttribute("data-pointer-focus");
    });
  });
  const loading = () =>
    !state.ready || !!state.openingProject || !!state.openingChat || !!state.loadingChat;
  const [palette, setPalette] = createSignal(false);
  const [filesVisited, setFilesVisited] = createSignal(false);
  const [dock, setDock] = createSignal<Dock | "tools" | null>(null);
  const [dockTabs, setDockTabs] = createSignal<Dock[]>([]);
  const [sidebarVisible, setSidebarVisible] = createSignal(true);
  const [sidebarScrolling, setSidebarScrolling] = createSignal(false);
  let sidebarScrollTimer: ReturnType<typeof setTimeout> | undefined;
  function showSidebarScrollbar() {
    setSidebarScrolling(true);
    clearTimeout(sidebarScrollTimer);
    sidebarScrollTimer = setTimeout(() => setSidebarScrolling(false), 900);
  }
  onCleanup(() => clearTimeout(sidebarScrollTimer));
  const [sidebarWidth, setSidebarWidth] = createSignal(258);
  /* The side panel opens beside the conversation, not over it: wide enough to
     read a page or a diff, narrow enough that the chat keeps most of its
     measure. Dragging it is remembered, so this is only the starting split. */
  const [dockWidth, setDockWidth] = createSignal(460);
  const [terminalHeight, setTerminalHeight] = createSignal(260);
  const [dockExpanded, setDockExpanded] = createSignal(false);
  /** Whether the sidebar groups chats under their project or lists them flat. */
  const [sidebarLayout, setSidebarLayout] = createSignal<"project" | "flat">(
    "project",
  );
  const [layoutReady, setLayoutReady] = createSignal(false);
  // The flat list needs every project's index, which the store decides.
  createEffect(() => set("sidebarLayout", sidebarLayout()));
  let workbench!: HTMLDivElement;
  const clamp = (value: number, min: number, max: number) =>
    Math.max(min, Math.min(max, value));
  const dockMax = () => Math.max(360, (workbench?.clientWidth ?? 1050) - 480);
  const dockVisible = () =>
    !!state.connection && !!dock() && state.panel === "chat";
  function openDock(value: Dock) {
    setDock(value);
    if (!dockTabs().includes(value)) setDockTabs((tabs) => [...tabs, value]);
    if (value === "files") setFilesVisited(true);
    set("panel", "chat");
  }
  function closeDock(value: Dock) {
    setDockTabs((tabs) => tabs.filter((tab) => tab !== value));
    if (dock() === value) setDock(dockTabs().at(-1) ?? "tools");
    setDockExpanded(false);
  }
  createEffect(() => {
    if (state.panel === "files" || state.panel === "diff")
      openDock(state.panel);
  });
  onMount(async () => {
    const raw = (await window.haarness.storage("layout:v2")) as Record<
      string,
      unknown
    > | null;
    if (raw) {
      if (typeof raw.sidebarVisible === "boolean")
        setSidebarVisible(raw.sidebarVisible);
      if (
        typeof raw.sidebarWidth === "number" &&
        Number.isFinite(raw.sidebarWidth)
      )
        setSidebarWidth(clamp(raw.sidebarWidth, 210, 360));
      if (typeof raw.dockWidth === "number" && Number.isFinite(raw.dockWidth))
        setDockWidth(clamp(raw.dockWidth, 360, 1100));
      if (
        typeof raw.terminalHeight === "number" &&
        Number.isFinite(raw.terminalHeight)
      )
        setTerminalHeight(clamp(raw.terminalHeight, 160, 600));
      if (raw.sidebarLayout === "flat" || raw.sidebarLayout === "project")
        setSidebarLayout(raw.sidebarLayout);
      if (["files", "diff", "context", "browser"].includes(String(raw.dock)))
        openDock(raw.dock as Dock);
      else if (raw.dock === "tools") setDock("tools");
    }
    setLayoutReady(true);
  });
  createEffect(() => {
    if (!layoutReady()) return;
    const layout = {
      sidebarVisible: sidebarVisible(),
      sidebarWidth: sidebarWidth(),
      dockWidth: dockWidth(),
      terminalHeight: terminalHeight(),
      dock: dock(),
      sidebarLayout: sidebarLayout(),
    };
    const timer = setTimeout(
      () => void window.haarness.storage("layout:v2", layout),
      200,
    );
    onCleanup(() => clearTimeout(timer));
  });
  const projects = createOrdering(
    () => "list:projects",
    () => app.projects().map((item) => item.id),
  );
  const unfiledDrop = createOrderDropTarget(
    () => "list:projects",
    () => "section:",
    (source) => void app.run(() => app.fileProjectInSection(source, "")),
  );
  /** Each section under its name, then the projects filed under none. */
  const groups = createMemo(() =>
    sidebarGroups(projects.ordered(), state.projectMeta),
  );
  const sessionTabs = createOrdering(
    () => "list:session-tabs:" + state.connection?.id,
    () => state.tabs,
  );
  const panelTabs = createOrdering(
    () => "list:panel-tabs",
    () => dockTabs(),
  );
  const selected = () => state.threads.find((t) => t.id === state.selected);
  const running = (id: string) =>
    state.turns.some((turn) => turn.threadId === id);
  async function newChat() {
    await app.startChat();
    document
      .querySelector<HTMLTextAreaElement>("[data-composer-input]")
      ?.focus();
  }
  async function open() {
    await app.openProject();
  }
  /** The panel owns startup for every entry point, including keyboard toggles. */
  function openTerminal() {
    set("terminal", true);
    setDockExpanded(false);
  }
  const remote = openServerConnection;
  app.registerSearch("application", () => {
    const connected = !!state.connection && !state.openingProject;
    const current = selected();
    const project = app.currentProject();
    const location = app.project();
    const hint = (key: string, fallback = "") =>
      shortcutLabel(
        state.preferences.shortcuts[key] ?? fallback,
        state.platform,
      );
    const actions: SearchEntry[] = [
      {key:"browser",group:"Project panels",name:t("Open browser"),keywords:"web website Chromium tabs",icon:"web",hint:hint("browser", "Mod+Shift+B"),disabled:!connected,action:()=>openDock("browser")},
      {key:"memory",group:"Conversation actions",name:t("Manage memory"),keywords:"project memory index",icon:"layers",disabled:!connected,action:()=>set("memoryOpen",true)},
      {
        key: "permission-mode",
        group: "Conversation actions",
        name: app.isAutoApproval()
          ? t("Switch approvals to Review")
          : t("Enable Auto approvals"),
        keywords: "permissions approval auto review allow once",
        icon: "shield",
        disabled: !connected,
        action: () => app.setAutoApproval(!app.isAutoApproval()),
      },
      {
        key: "new",
        group: "Commands",
        name: t("New session"),
        keywords: "new chat conversation",
        icon: "compose",
        hint: hint("new"),
        action: newChat,
      },
      {
        key: "open",
        group: "Commands",
        name: t("Open project"),
        keywords: "folder workspace directory",
        icon: "folder",
        hint: hint("open", "Mod+O"),
        action: open,
      },
      ...(["file", "folder"] as const).map(
        (kind): SearchEntry => ({
          key: kind + "-new",
          group: "Commands",
          name: kind === "file" ? "New file" : "New folder",
          icon: kind === "file" ? "file-new" : "folder-new",
          disabled: !connected,
          action: () => {
            set("fileRequest", {
              connection: state.connection!.id,
              path: "",
              action: kind,
              sequence: Date.now(),
            });
            openDock("files");
          },
        }),
      ),
      {
        key: "remote",
        group: "Commands",
        name: t("Connect to server"),
        keywords: "remote connection url host",
        icon: "cloud",
        action: remote,
      },
      {
        key: "files",
        group: "Commands",
        name: t("File explorer"),
        keywords: "browse edit files",
        icon: "folder",
        hint: hint("files"),
        disabled: !connected,
        action: () => openDock("files"),
      },
      {
        key: "diff",
        group: "Commands",
        name: t("Review changes"),
        keywords: "git diff code review",
        icon: "diff",
        hint: hint("diff"),
        disabled: !connected,
        action: () => openDock("diff"),
      },
      {
        key: "terminal",
        group: "Commands",
        name: t("Toggle terminal"),
        keywords: "shell console pty",
        icon: "terminal",
        hint: hint("terminal"),
        disabled: !connected,
        action: () => set("terminal", (value) => !value),
      },
      {
        key: "sidebar",
        group: "Commands",
        name: sidebarVisible() ? t("Hide sidebar") : t("Show sidebar"),
        icon: "sidebar",
        action: () => setSidebarVisible((value) => !value),
      },
      {
        key: "panel",
        group: "Commands",
        name: dockVisible() ? t("Hide side panel") : t("Show side panel"),
        icon: "panel",
        disabled: !connected,
        action: () => {
          set("panel", "chat");
          setDock(dockVisible() ? null : "tools");
        },
      },
      {
        key: "chat",
        group: "Commands",
        name: t("Return to conversation"),
        keywords: "back chat session",
        icon: "chat",
        disabled: !connected,
        action: () => set("panel", "chat"),
      },
      {
        key: "settings",
        group: "Commands",
        name: t("Settings"),
        keywords: "preferences configuration",
        icon: "settings",
        hint: hint("settings"),
        destination: "settings",
        action: () => app.openSettings("general"),
      },
      {
        key: "refresh",
        group: "Commands",
        name: t("Refresh project"),
        keywords: "reload reconnect resync",
        icon: "refresh",
        disabled: !connected,
        action: async () => {
          await app.refreshProjects();
          await app.hydrate();
        },
      },
      {
        key: "import",
        group: "Commands",
        name: t("Import session archive"),
        keywords: "restore history",
        icon: "download",
        disabled: !connected,
        action: async () => {
          const archive = await window.haarness.openArchive();
          if (archive) {
            await app.api("importSessionArchive", { archive });
            await app.refresh();
            app.notify("Session archive imported");
          }
        },
      },
      ...[
        ["new", "New window", "Mod+Shift+N"],
        ["zoom-in", "Zoom in", "Mod+Plus"],
        ["zoom-out", "Zoom out", "Mod+-"],
        ["zoom-reset", "Actual size", "Mod+0"],
        ["fullscreen", "Toggle full screen", ""],
        ["devtools", "Developer tools", ""],
      ].map(
        ([id, name, shortcut]): SearchEntry => ({
          key: "window:" + id,
          group: "Window",
          name,
          icon: "panel",
          hint: shortcutLabel(shortcut, state.platform),
          action: () => window.haarness.windowAction(id),
        }),
      ),
    ];
    if (current && connected)
      actions.push(
        {
          key: "conversation-context",
          group: "Conversation actions",
          name: t("View conversation context"),
          keywords: "usage tokens cache cost breakdown",
          icon: "chart",
          action: () => openDock("context"),
        },
        {
          key: "rename",
          group: "Conversation actions",
          name: t("Rename session"),
          icon: "edit",
          detail: current.title,
          action: () => renameSession(app, current),
        },
        {
          key: "fork",
          group: "Conversation actions",
          name: t("Fork session"),
          icon: "fork",
          detail: current.title,
          action: async () => {
            const fork = await app.api<{ id: string }>(
              "forkThread",
              current.id,
              {},
            );
            await app.refresh();
            await app.choose(fork.id);
          },
        },
        {
          key: "archive",
          group: "Conversation actions",
          name: current.archived ? "Restore session" : "Archive session",
          icon: current.archived ? "restore" : "archive",
          detail: current.title,
          action: async () => {
            await app.api("updateThread", current.id, {
              archived: !current.archived,
            });
            await app.refresh();
          },
        },
        {
          key: "close-chat",
          group: "Conversation actions",
          name: t("Close conversation tab"),
          icon: "close",
          detail: current.title,
          action: () => app.closeTab(current.id),
        },
      );
    if (project?.git && !project.remote && connected)
      actions.push(
        {
          key: "worktree-new",
          group: "Worktree actions",
          name: t("New worktree"),
          icon: "branch",
          detail: project.label,
          action: () => createWorktree(app, primaryLocation(project).id),
        },
        {
          key: "worktree-view",
          group: "Worktree actions",
          name: state.worktreeViews[project.id]
            ? "Disable worktree view"
            : "Enable worktree view",
          icon: "branch",
          detail: project.label,
          action: () =>
            app.setWorktreeView(project.id, !state.worktreeViews[project.id]),
        },
      );
    if (project && location?.checkout?.managed && connected)
      actions.push(
        {
          key: "worktree-reset",
          group: "Worktree actions",
          name: t("Reset worktree"),
          icon: "refresh",
          detail: t(locationName(location)),
          action: () => resetWorktree(app, project, location),
        },
        {
          key: "worktree-remove",
          group: "Worktree actions",
          name: t("Remove worktree"),
          icon: "trash",
          detail: t(locationName(location)),
          action: () => removeWorktree(app, project, location),
        },
      );
    return actions;
  });
  function dispatch(value: unknown) {
    if (typeof value === "object" && value) {
      const input = value as { action: string; url: string };
      if (input.action === "preferences") {
        set(
          "preferences",
          (value as { value: typeof state.preferences }).value,
        );
        return;
      }
      if (input.action === "deep-link")
        void app.run(async () => {
          const url = new URL(input.url);
          const id = url.searchParams.get("session");
          const workspace = url.searchParams.get("workspace");
          if (workspace) await app.connect({ workspace });
          if (id && state.threads.some((t) => t.id === id))
            await app.choose(id);
        });
      return;
    }
    if (value === "new") void app.run(newChat);
    else if (value === "open") void app.run(open);
    else if (
      value === "settings" ||
      value === "shortcuts" ||
      value === "about"
    )
      void app.run(() =>
        app.openSettings(value === "settings" ? "general" : value),
      );
    else if (value === "palette") openPalette();
    else if (value === "browser") openDock("browser");
    else if (value === "terminal") set("terminal", (v) => !v);
    else if (value === "files" || value === "diff") set("panel", value);
  }
  function openPalette() {
    if (!document.querySelector("dialog[open]:not(.settings-dialog):not(.palette-dialog)"))
      setPalette((value) => !value);
  }
  app.onMenu(dispatch);
  onMount(() => {
    void app.run(app.boot);
    const keys = (event: KeyboardEvent) => {
      // A nested picker/menu owns Escape. Solid delegates its handlers at the
      // document, so this listener must not dismiss the containing settings first.
      if (
        event.defaultPrevented ||
        (event.target instanceof Element &&
          event.target.closest(
            ".select-popover, .action-menu, .project-popover",
          ))
      )
        return;
      if (event.key === "Escape") {
        if (palette()) {
          setPalette(false);
          return;
        }
        if (state.settings && !document.querySelector("dialog[open]"))
          void app.closeSettings();
      }
    };
    document.addEventListener("keydown", keys);
    onCleanup(() => document.removeEventListener("keydown", keys));
  });
  return (
    <div
      class="application"
      data-project-ready={
        state.ready &&
        !state.openingProject &&
        state.connectionStatus === "Connected"
      }
      data-platform={state.platform}
      style={{
        "--sidebar-width": sidebarVisible() ? sidebarWidth() + "px" : "0px",
        "--dock-width": dockWidth() + "px",
        "--terminal-height": terminalHeight() + "px",
      }}
    >
      <header class="titlebar">
        <div class="window-toolbar">
          <Button
            icon="sidebar"
            label={sidebarVisible() ? t("Hide sidebar") : t("Show sidebar")}
            active={sidebarVisible()}
            onClick={() => setSidebarVisible((value) => !value)}
          />
        </div>
        <div class="titlebar-session">
          <Show
            when={state.connection}
            fallback={<span class="titlebar-caption">{t("Haarness")}</span>}
          >
            <Icon name="folder" size="sm" />
            <span class="titlebar-caption">
              {app.currentProject()?.label ?? app.project()?.label}
            </span>
            <Show when={isWorktree(app.project())}>
              <span class="checkout-badge" title={app.project()?.workspace}>
                <Icon name="branch" size="xs" />
                {/* A bare text node in a flex row cannot take the ellipsis. */}
                <span>{t(locationName(app.project()))}</span>
              </span>
            </Show>
            <Show when={state.connection!.remote}>
              <span class="badge">{t("Remote")}</span>
            </Show>
          </Show>
        </div>
        <div class="titlebar-actions">
          <Show when={state.connection}>
            <Button
              icon="terminal"
              label={t("Toggle terminal")}
              disabled={!state.connection}
              active={state.terminal}
              onClick={() => set("terminal", (value) => !value)}
            />
            <span class="titlebar-divider" />
            <Button
              icon="panel"
              label={dockVisible() ? t("Hide side panel") : t("Show side panel")}
              disabled={!state.connection}
              active={dockVisible()}
              onClick={() =>
                dockVisible()
                  ? setDock(null)
                  : setDock(dockTabs().at(-1) ?? "tools")
              }
            />
          </Show>
        </div>
      </header>
      <div class="app-body">
        <aside
          class="sidebar"
          style={{ display: sidebarVisible() ? "flex" : "none" }}
        >
          <div class="sidebar-brand">
            <Wordmark class="wordmark" />
          </div>
          <div class="server-switcher">
            <MenuButton
              label={t("Switch server")}
              class="server-switcher-button"
              content={
                <>
                  <Icon name="link" size="md" />
                  <span>{t(app.currentServer().label)}</span>
                  <Icon name="down" size="xs" />
                </>
              }
              items={[
                ...app.servers().map((server) => ({
                  label: t(server.label),
                  detail: server.url ?? "Local projects",
                  icon: (server.id === state.activeServer
                    ? "check"
                    : "link") as IconName,
                  run: () => void app.run(() => app.switchServer(server.id)),
                })),
                {
                  label: t("Manage servers"),
                  icon: "settings",
                  run: () => {
                    void app.openSettings("servers");
                  },
                },
              ]}
            />
          </div>
          <nav class="primary-nav" aria-label={t("Main navigation")}>
            <button
              aria-label={t("New session")}
              class="new-session"
              onClick={() => app.run(newChat)}
            >
              <Icon name="compose" size="md" />
              {t("New chat")}
              <kbd>
                {shortcutLabel(
                  state.preferences.shortcuts.new,
                  state.platform,
                )}
              </kbd>
            </button>
          </nav>
          <div class="nav-group">
            <button class="nav-row" aria-label={t("Search")} onClick={openPalette}>
              <Icon name="search" size="md" />
              {t("Search")}
              <kbd>
                {shortcutLabel(
                  state.preferences.shortcuts.palette,
                  state.platform,
                )}
              </kbd>
            </button>
          </div>
          <div class="section-label">
            <span>{t("Projects")}</span>
            <div class="section-label-actions">
              <Button
                icon="plus"
                label={t("Open another project")}
                onClick={() => app.run(open)}
              />
              <MenuButton
                icon="more"
                label={t("Project list options")}
                items={[
                  {
                    label: t("Organize sidebar"),
                    submenu: [
                      {
                        label: t("By project"),
                        checked: sidebarLayout() === "project",
                        run: () => setSidebarLayout("project"),
                      },
                      {
                        label: t("In one list"),
                        checked: sidebarLayout() === "flat",
                        run: () => setSidebarLayout("flat"),
                      },
                    ],
                  },
                ]}
              />
            </div>
          </div>
          <div
            class="sidebar-lists"
            classList={{ "is-scrolling": sidebarScrolling() }}
            onScroll={showSidebarScrollbar}
          >
            <div class="project-list" {...unfiledDrop}>
              <Show
                when={projects.ordered().length}
                fallback={
                  <button
                    class="project-heading"
                    onClick={() => app.run(open)}
                  >
                    <Icon name="folder" size="md" />
                    <span>{t("Open project")}</span>
                  </button>
                }
              >
                <Show
                  when={sidebarLayout() === "project"}
                  fallback={<SessionList />}
                >
                  <For each={groups().sections.map((section) => section.name)}>
                    {(name) => (
                      <ProjectSection
                        name={name}
                        ids={
                          groups().sections.find(
                            (section) => section.name === name,
                          )?.ids ?? []
                        }
                      />
                    )}
                  </For>
                  {/* Once anything is filed, the remainder needs a name too,
                      or it reads as the tail of the last section. */}
                  <Show
                    when={groups().sections.length}
                    fallback={<ProjectRows ids={groups().rest} />}
                  >
                    <ProjectSection
                      name={t("No section")}
                      ids={groups().rest}
                      fixed
                    />
                  </Show>
                </Show>
              </Show>
            </div>
          </div>
          <footer class="sidebar-footer">
            <button
              class="nav-row"
              aria-label={t("Settings")}
              onClick={() => set("settings", "general")}
            >
              <Icon name="settings" size="md" />
              {t("Settings")}
            </button>
          </footer>
        </aside>
        <Show when={sidebarVisible()}>
          <ResizeHandle
            label={t("Resize sidebar")}
            value={sidebarWidth()}
            min={210}
            max={360}
            onDelta={(delta) =>
              setSidebarWidth((value) => clamp(value + delta, 210, 360))
            }
            onReset={() => setSidebarWidth(258)}
          />
        </Show>
        <main class="workspace-main">
          <div
            class="workbench"
            ref={workbench}
            classList={{
              "has-dock": dockVisible(),
              "dock-expanded": dockVisible() && dockExpanded(),
            }}
          >
            <div class="chat-pane">
              <Show when={state.panel === "chat" && !!state.selected && !loading()}>
                <div class="session-bar">
                  <Show when={selected()?.isSubagent && selected()?.parentId}>
                    <Button
                      label={t("Back to parent conversation")}
                      icon="chevron"
                      class="parent-conversation"
                      onClick={() =>
                        app.run(() => app.choose(selected()!.parentId!))
                      }
                    />
                  </Show>
                  <Show
                    when={state.tabs.length > 1}
                    fallback={
                      <div class="session-bar-title">
                        <Show
                          when={selected()}
                          fallback={<span class="muted">{t("New chat")}</span>}
                        >
                          <Show when={running(state.selected)}>
                            <span class="session-status running" />
                          </Show>
                          {/* Rename also lives in the session menu, so this
                                shortcut stays out of the tab order. */}
                          <span
                            class="session-bar-name"
                            title={t("Double-click to rename")}
                            onDblClick={() =>
                              app.run(() => renameSession(app, selected()!))
                            }
                          >
                            {selected()!.title || "Untitled session"}
                          </span>
                        </Show>
                      </div>
                    }
                  >
                    <div class="workspace-tabs" role="tablist">
                      <For each={sessionTabs.ordered()}>
                        {(id) => (
                          <div
                            class="workspace-tab"
                            {...sessionTabs.dropProps(
                              id,
                              t("session tab"),
                              "x",
                            )}
                            classList={{
                              active:
                                id === state.selected &&
                                state.panel === "chat",
                            }}
                          >
                            <button
                              role="tab"
                              aria-selected={id === state.selected}
                              onClick={() => app.run(() => app.choose(id))}
                            >
                              <Show when={running(id)}>
                                <span class="session-status running" />
                              </Show>
                              {state.threads.find(
                                (thread) => thread.id === id,
                              )?.title ?? "Session"}
                            </button>
                            <Button
                              icon="close"
                              label={t("Close session tab")}
                              onClick={() => app.run(() => app.closeTab(id))}
                            />
                          </div>
                        )}
                      </For>
                    </div>
                  </Show>
                  <div class="session-bar-actions">
                    <Show when={selected()}>
                      <SessionActions
                        thread={selected()!}
                        label={t("Session actions")}
                      />
                    </Show>
                    <Button
                      icon="plus"
                      label={t("New session tab")}
                      onClick={() => app.run(newChat)}
                    />
                  </div>
                </div>
              </Show>
              <div class="workspace-content">
                <Show when={state.panel === "chat" && loading()}>
                  <ChatLoading
                    label={
                      state.openingChat || state.loadingChat
                        ? "Loading conversation…"
                        : state.openingProject ? t("Opening project…") : t("Loading…")
                    }
                  />
                </Show>
                <Show when={state.panel === "chat" && !state.connection && !loading()}>
                  <ProjectHome />
                </Show>
                <div
                  class="persistent-panel"
                  style={{
                    display:
                      state.panel === "chat" && state.connection && !loading()
                        ? "flex"
                        : "none",
                  }}
                >
                  <Session />
                </div>
              </div>
            </div>
            <Show when={dockVisible() && !dockExpanded()}>
              <ResizeHandle
                label={t("Resize side panel")}
                value={dockWidth()}
                min={360}
                max={dockMax()}
                onDelta={(delta) =>
                  setDockWidth((value) =>
                    clamp(value - delta, 360, dockMax()),
                  )
                }
                onReset={() => setDockWidth(530)}
              />
            </Show>
            <aside
              class="side-panel"
              aria-label={t("Project side panel")}
              style={{ display: dockVisible() ? "flex" : "none" }}
            >
              <div class="dock-tabs">
                <For each={panelTabs.ordered() as Dock[]}>
                  {(tab) => (
                    <div
                      class="dock-tab"
                      {...panelTabs.dropProps(
                        tab,
                        t("{name} tab", { name: t(dockNames[tab]) }),
                        "x",
                      )}
                      classList={{ active: tab === dock() }}
                    >
                      <button
                        aria-label={t("Show {name} panel", { name: tab })}
                        onClick={() => openDock(tab)}
                      >
                        <Icon name={dockIcons[tab]} size="sm" />
                        {dockNames[tab]}
                      </button>
                      <Button
                        icon="close"
                        label={t("Close {name} panel", { name: tab })}
                        onClick={() => closeDock(tab)}
                      />
                    </div>
                  )}
                </For>
                <Button
                  icon="plus"
                  label={t("Open panel")}
                  active={dock() === "tools"}
                  onClick={() => setDock("tools")}
                />
                <div class="grow" />
                <Button
                  icon={dockExpanded() ? "shrink" : "expand"}
                  label={
                    dockExpanded()
                      ? "Restore side panel"
                      : "Expand side panel"
                  }
                  onClick={() => setDockExpanded((value) => !value)}
                />
              </div>
              <Show when={dock() === "tools"}>
                <section
                  class="panel-launcher"
                  aria-label={t("Open a project panel")}
                >
                  <h2>{t("Your tools, alongside chat")}</h2>
                  <p>{t("Open what you need without leaving the conversation.")}</p>
                  <For each={panelChoices}>
                    {(item) => (
                      <button
                        onClick={() =>
                          app.run(async () => {
                            if (item.shortcut === "terminal")
                              await openTerminal();
                            else openDock(item.shortcut as Dock);
                          })
                        }
                        aria-label={t("Open {name}", { name: t(item.name) })}
                      >
                        <Icon name={item.icon} size="xl" />
                        <span>
                          {t(item.name)}
                          <small>{t(item.detail)}</small>
                        </span>
                        <kbd>
                          {state.preferences.shortcuts[item.shortcut]
                            ?.replace(
                              "Mod",
                              state.platform === "darwin" ? "⌘" : "Ctrl",
                            )
                            .replaceAll("+", " ")}
                        </kbd>
                      </button>
                    )}
                  </For>
                </section>
              </Show>
              <div
                class="persistent-panel dock-content"
                style={{ display: dock() === "files" ? "flex" : "none" }}
              >
                <Show when={state.connection && filesVisited()}>
                  <Suspense
                    fallback={<PanelLoading label={t("Opening files…")} />}
                  >
                    <Explorer />
                  </Suspense>
                </Show>
              </div>
              <Show when={state.connection && dock() === "diff"}>
                <Review compact />
              </Show>
              <Show when={state.connection && dock() === "context"}>
                <ConversationContext />
              </Show>
              <BrowserPanel visible={dockVisible() && dock() === "browser" && !palette()} onOpen={() => openDock("browser")} />
            </aside>
          </div>
          <Show when={state.connection && state.terminal}>
            <ResizeHandle
              label={t("Resize terminal")}
              orientation="horizontal"
              value={terminalHeight()}
              min={160}
              max={600}
              onDelta={(delta) =>
                setTerminalHeight((value) =>
                  clamp(
                    value - delta,
                    160,
                    Math.min(600, window.innerHeight - 320),
                  ),
                )
              }
              onReset={() => setTerminalHeight(260)}
            />
            <div class="terminal-dock">
              <Suspense fallback={<PanelLoading label={t("Opening terminal…")} />}>
                <TerminalPanel />
              </Suspense>
            </div>
          </Show>
        </main>
      </div>
      <Show when={!state.settings}><Notifications /></Show>
      <Show when={state.settings}>
        <Settings />
      </Show>
      <Show when={state.memoryOpen && state.connection}>
        <MemoryDialog close={()=>set("memoryOpen",false)} />
      </Show>
      <Show when={state.connection && state.connect}>
        {(options) => (
          <ConnectProvider
            provider={options().provider}
            onboarding={options().onboarding}
            close={() => set("connect", null)}
          />
        )}
      </Show>
      <Show when={state.remoteProjectRequest}>
        {(request) => (
          <RemoteProjectPicker
            server={request().server}
            newChat={request().newChat}
            close={() => set("remoteProjectRequest", null)}
          />
        )}
      </Show>
      <Forms />
      <ServerConnect />
      <Show when={palette()}>
        <UniversalSearch close={() => setPalette(false)} />
      </Show>
    </div>
  );
}
function PanelLoading(props: { label: string }) {
  return (
    <div class="panel-loading">
      <span class="spinner" />
      {props.label}
    </div>
  );
}
