import { BrowserPanel } from "./BrowserPanel";
import { AccountMenu } from "./AccountMenu";
import { Notifications } from "./Notifications";
import { t } from "./i18n";
import { ServerConnect, openServerConnection } from "./ServerConnect";
import { RemoteProjectPicker } from "./RemoteProjectPicker";
import { ConnectProvider } from "./ConnectProvider";
import { RemoteControlDialog } from "./RemoteControlDialog";
import {
  For,
  Show,
  createEffect,
  createMemo,
  lazy,
  on,
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
import { Button, Icon, Forms, type IconName } from "./ui";
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
import { matchesShortcut, shortcutLabel, type SearchEntry } from "./search";
import { registerCyclePanel, stepFocusedPanel, tabButton } from "./tab-cycle";
import { keyPlace, overlayOpen, stepIndex, stepsChats } from "./keyboard-nav";
import { SelectionBar } from "./SelectionBar";
import { selection } from "./sidebar-selection";
import { createWorktree, resetWorktree, removeWorktree } from "./worktrees";
import { parseDeepLink } from "../deep-link";
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
    detail: "Inspect chat tokens, cache and context",
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
import { chatTitle } from "./chat-title";
const TerminalPanel = lazy(() =>
  import("./Terminal").then((module) => ({ default: module.TerminalPanel })),
);
const MemoryDialog = lazy(() => import("./MemoryDialog").then((module) => ({default: module.MemoryDialog})));
const Settings = lazy(() =>
  import("./Settings").then((module) => ({ default: module.Settings })),
);
/** This computer, a remote host, or Haarness Cloud, each with its own picture. */
const serverIcon = (server: { url?: string; cloud?: boolean }): IconName =>
  server.cloud ? "cloud" : server.url ? "server" : "computer";
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
  createEffect(() => {
    const request = state.fileRequest;
    if ((request?.preview || request?.link) && request.connection === state.connection?.id && (!request.thread || request.thread === state.selected)) openDock("files");
  });
  /** The agent took the Browser's tab to another page while another panel held the side panel. */
  const [browserMoved, setBrowserMoved] = createSignal(false);
  createEffect(() => {
    if (dock() === "browser") setBrowserMoved(false);
  });
  // The dot is about the shown chat's browser; another chat starts without it.
  createEffect(
    on(
      () => [state.connection?.id, state.selected],
      () => setBrowserMoved(false),
      { defer: true },
    ),
  );
  /**
   * The agent took the tab the Browser shows to another page. A closed side
   * panel (or one showing only the panel launcher) opens on the Browser,
   * also after the user closed it; another panel keeps the side panel and
   * the Browser tab gets a dot instead. Unlike a tab the agent opens, this
   * never leaves another page for the chat.
   */
  function revealBrowser() {
    if (dock() === "browser") return;
    if (!dockTabs().includes("browser"))
      setDockTabs((tabs) => [...tabs, "browser"]);
    if (dock() === null || dock() === "tools") setDock("browser");
    else setBrowserMoved(true);
  }
  function closeDock(value: Dock) {
    if (value === "browser") setBrowserMoved(false);
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
  // Ctrl+Tab in the conversation steps the chat tabs, and elsewhere in the
  // side panel its tabs (Files, Review, Browser…), unless a panel inside it
  // has tabs of its own (see tab-cycle.ts).
  onCleanup(
    registerCyclePanel({
      id: "chats",
      tabs: () =>
        state.panel === "chat" && state.selected ? sessionTabs.ordered() : [],
      active: () => state.selected,
      select: (id) => void app.run(() => app.choose(id)),
      // The chat loads (its message box and tabs hidden meanwhile, which
      // drops the focus) before the keyboard goes back: to the message box
      // when the step started there, else to the chat's tab.
      keep: (id, from) => {
        const composer = !!from?.matches("[data-composer-input]");
        let frames = 300;
        const land = () => {
          if (state.selected !== id) return;
          if (loading() && frames-- > 0) return void setTimeout(land, 16);
          if (composer)
            document.querySelector<HTMLElement>("[data-composer-input]")?.focus();
          else if (!from?.isConnected || from.closest("[data-cycle-tab]") || document.activeElement !== from)
            tabButton(document.querySelector(".session-bar"), id)?.focus();
        };
        land();
      },
    }),
  );
  onCleanup(
    registerCyclePanel({
      id: "dock",
      tabs: () => (dockVisible() ? panelTabs.ordered() : []),
      active: () => dock() ?? undefined,
      select: (tab) => openDock(tab as Dock),
    }),
  );
  onMount(() => {
    const step = (event: KeyboardEvent) => {
      const { tabNext, tabPrev } = state.preferences.shortcuts;
      const offset = matchesShortcut(event, tabNext, state.platform)
        ? 1
        : matchesShortcut(event, tabPrev, state.platform)
          ? -1
          : 0;
      if (!offset || event.defaultPrevented) return;
      // Before the terminal, the editor or a tab list hears it, so the shell
      // gets nothing and the step runs once.
      if (stepFocusedPanel(document.activeElement, offset)) {
        event.preventDefault();
        event.stopPropagation();
      }
    };
    window.addEventListener("keydown", step, true);
    onCleanup(() => window.removeEventListener("keydown", step, true));
  });
  /** The chat rows the sidebar draws, top to bottom: rows inside a collapsed
   * project, checkout or schedule are not drawn and are passed over. */
  const chatRows = () => [
    ...document.querySelectorAll<HTMLButtonElement>(
      ".sidebar .session-row > button[aria-current]",
    ),
  ];
  /** Opens the chat `offset` rows from the open one, wrapping. The keyboard
   * goes back to the message box when the step started there. */
  function stepChat(offset: number, from: Element | null) {
    // In Select chats a row click picks it instead of opening it.
    if (selection()) return;
    const rows = chatRows();
    const at = rows.findIndex((row) => row.getAttribute("aria-current") === "true");
    const target = rows[stepIndex(rows.length, at, offset) ?? -1];
    if (!target || target === rows[at]) return;
    target.click();
    if (!from?.matches("[data-composer-input]")) return;
    let frames = 300;
    const land = () => {
      if (loading() && frames-- > 0) return void setTimeout(land, 16);
      document.querySelector<HTMLElement>("[data-composer-input]")?.focus();
    };
    setTimeout(land);
  }
  /** Shows the sidebar and puts the keyboard on the open chat's row, opening
   * its project and checkout when they are collapsed. */
  async function focusSidebar() {
    setSidebarVisible(true);
    const project = app.currentProject();
    if (project) await app.expandProject(project.id, true);
    const location = app.project();
    if (location && app.state.expandedLocations[location.id] === false)
      await app.expandLocation(location.id, true);
    setTimeout(() => {
      const row =
        chatRows().find((row) => row.getAttribute("aria-current") === "true") ??
        chatRows()[0] ??
        document.querySelector<HTMLElement>(".sidebar-lists button");
      row?.focus();
      row?.scrollIntoView({ block: "nearest" });
    });
  }
  onMount(() => {
    const keys = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing) return;
      const { chatPrev, chatNext, focusSidebar: focus } = state.preferences.shortcuts;
      const offset =
        chatNext && matchesShortcut(event, chatNext, state.platform)
          ? 1
          : chatPrev && matchesShortcut(event, chatPrev, state.platform)
            ? -1
            : 0;
      const focusing = !!focus && matchesShortcut(event, focus, state.platform);
      if (!offset && !focusing) return;
      const from = document.activeElement;
      const draft =
        document.querySelector<HTMLTextAreaElement>("[data-composer-input]")?.value ?? "";
      const place = keyPlace(from, draft, overlayOpen(document));
      if (focusing ? place === "overlay" : !stepsChats(place)) return;
      event.preventDefault();
      if (focusing) void app.run(focusSidebar);
      else stepChat(offset, from);
    };
    // After the page's own handlers: a sidebar row that has the keyboard
    // moves itself with Alt and an arrow and stops the key there.
    window.addEventListener("keydown", keys);
    onCleanup(() => window.removeEventListener("keydown", keys));
  });
  const selected = () => state.threads.find((t) => t.id === state.selected);
  /** A chat tab's name, read where it is shown so a generated title or a
   * rename reaches an open tab. */
  const tabTitle = (id: string) => {
    const thread = state.threads.find((item) => item.id === id);
    return thread ? chatTitle(thread.title) : t("Chat");
  };
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
      {key:"memory",group:"Chat actions",name:t("Manage memory"),keywords:"project memory index",icon:"layers",disabled:!connected,action:()=>set("memoryOpen",true)},
      {
        key: "permission-mode",
        group: "Chat actions",
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
        name: t("New chat"),
        keywords: "new chat conversation session",
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
          name: kind === "file" ? t("New file") : t("New folder"),
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
        key: "focus-sidebar",
        group: "Commands",
        name: t("Focus sidebar"),
        keywords: "keyboard chats list navigate",
        icon: "sidebar",
        hint: hint("focusSidebar"),
        action: focusSidebar,
      },
      {
        key: "chat-previous",
        group: "Commands",
        name: t("Previous chat"),
        keywords: "sidebar step up conversation session",
        icon: "move-up",
        hint: hint("chatPrev"),
        action: () => stepChat(-1, null),
      },
      {
        key: "chat-next",
        group: "Commands",
        name: t("Next chat"),
        keywords: "sidebar step down conversation session",
        icon: "move-down",
        hint: hint("chatNext"),
        action: () => stepChat(1, null),
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
        name: t("Return to chat"),
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
        name: t("Import chat archive"),
        keywords: "restore history",
        icon: "download",
        disabled: !connected,
        action: async () => {
          const archive = await window.haarness.openArchive();
          if (archive) {
            await app.api("importSessionArchive", { archive });
            await app.refresh();
            app.notify(t("Chat archive imported"));
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
          name: t(name),
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
          group: "Chat actions",
          name: t("View chat context"),
          keywords: "usage tokens cache cost breakdown",
          icon: "chart",
          action: () => openDock("context"),
        },
        {
          key: "rename",
          group: "Chat actions",
          name: t("Rename chat"),
          icon: "edit",
          detail: chatTitle(current.title),
          action: () => renameSession(app, current),
        },
        {
          key: "fork",
          group: "Chat actions",
          name: t("Fork chat"),
          icon: "fork",
          detail: chatTitle(current.title),
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
          group: "Chat actions",
          name: current.archived ? t("Restore chat") : t("Archive chat"),
          icon: current.archived ? "restore" : "archive",
          detail: chatTitle(current.title),
          action: async () => {
            await app.api("updateThread", current.id, {
              archived: !current.archived,
            });
            await app.refresh();
          },
        },
        {
          key: "close-chat",
          group: "Chat actions",
          name: t("Close chat tab"),
          icon: "close",
          detail: chatTitle(current.title),
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
            ? t("Disable worktree view")
            : t("Enable worktree view"),
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
          const link = parseDeepLink(input.url);
          if (!link)
            return app.notify(t("This Haarness link could not be opened."), "blocked");
          if (link.workspace) {
            // A folder already in the sidebar opens as it is; the main process
            // confirms any other and opens it untrusted.
            const saved = state.recent.find(
              (item) => !item.remote && item.workspace === link.workspace,
            );
            const opened = saved
              ? await app.activateProject(saved.id)
              : await app.connect({ workspace: link.workspace, deepLink: true });
            if (!opened) return;
          }
          // Only a chat in the project now open; other projects are not searched.
          if (link.session && state.threads.some((t) => t.id === link.session))
            await app.choose(link.session);
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
              <span class="badge">{app.currentServer().cloud ? t("Cloud session") : t("Remote")}</span>
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
              matchWidth
              content={
                <>
                  <Icon name={serverIcon(app.currentServer())} size="md" />
                  <span>{t(app.currentServer().label)}</span>
                  <Icon name="down" size="xs" />
                </>
              }
              items={[
                ...app.servers().map((server) => ({
                  label: t(server.label),
                  detail:
                    server.url ??
                    t(server.cloud ? "Cloud sessions" : "Local projects"),
                  icon: (server.id === state.activeServer
                    ? "check"
                    : serverIcon(server)) as IconName,
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
              aria-label={t("New chat")}
              class="new-session"
              onClick={() => app.run(newChat)}
            >
              <Icon name="compose" size="md" />
              {t("New chat")}
              <kbd dir="ltr">
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
              <kbd dir="ltr">
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
          <SelectionBar />
          <footer class="sidebar-footer">
            <AccountMenu />
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
            <div class="chat-pane" data-cycle-panel="chats">
              <Show when={state.panel === "chat" && !!state.selected && !loading()}>
                <div class="session-bar">
                  <Show when={selected()?.isSubagent && selected()?.parentId}>
                    <Button
                      label={t("Back to parent chat")}
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
                            title={
                              chatTitle(selected()!.title) +
                              "\n" +
                              t("Double-click to rename")
                            }
                            onDblClick={() =>
                              app.run(() => renameSession(app, selected()!))
                            }
                          >
                            {chatTitle(selected()!.title)}
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
                              t("chat tab"),
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
                              data-cycle-tab={id}
                              aria-selected={id === state.selected}
                              title={tabTitle(id)}
                              onClick={() => app.run(() => app.choose(id))}
                            >
                              <Show when={running(id)}>
                                <span class="session-status running" />
                              </Show>
                              {tabTitle(id)}
                            </button>
                            <Button
                              icon="close"
                              label={t("Close chat tab")}
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
                        label={t("Chat actions")}
                      />
                    </Show>
                    <Button
                      icon="plus"
                      label={t("New chat tab")}
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
                        ? t("Loading chat…")
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
              data-cycle-panel="dock"
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
                        data-cycle-tab={tab}
                        aria-current={tab === dock() ? "true" : undefined}
                        aria-label={t("Show {name} panel", { name: tab })}
                        aria-description={
                          tab === "browser" && browserMoved()
                            ? t("The agent opened another page")
                            : undefined
                        }
                        onClick={() => openDock(tab)}
                      >
                        <Icon name={dockIcons[tab]} size="sm" />
                        {t(dockNames[tab])}
                        <Show when={tab === "browser" && browserMoved()}>
                          <span class="dock-tab-dot" aria-hidden="true" />
                        </Show>
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
                      ? t("Restore side panel")
                      : t("Expand side panel")
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
                  <p>{t("Open what you need without leaving the chat.")}</p>
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
                        <kbd dir="ltr">
                          {shortcutLabel(
                            state.preferences.shortcuts[item.shortcut] ?? "",
                            state.platform,
                          )}
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
              <BrowserPanel visible={dockVisible() && dock() === "browser"} onOpen={() => openDock("browser")} onReveal={revealBrowser} />
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
      <Show when={state.connection && state.selected ? state.remoteControl : null}>
        {(request) => (
          <RemoteControlDialog
            action={request().action}
            close={() => set("remoteControl", null)}
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
