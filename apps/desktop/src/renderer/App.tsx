import { BrowserPanel } from "./BrowserPanel";
import { AccountMenu } from "./AccountMenu";
import { UpdateRow } from "./Updates";
import { Notifications } from "./Notifications";
import { formatNumber, t } from "./i18n";
import { ServerConnect, openServerConnection } from "./ServerConnect";
import { RemoteProjectPicker } from "./RemoteProjectPicker";
import { ConnectProvider } from "./ConnectProvider";
import { RemoteControlDialog } from "./RemoteControlDialog";
import { RemoteAccessButton } from "./RemoteAccessButton";
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
import { ImageViewer } from "./ImageViewer";
import { Session } from "./Session";
import { ProjectHome } from "./ProjectHome";
import { accountKnown, computers, lastSeen, loadComputers, platformName, signedIn } from "./computers";
import { ChatLoading } from "./ChatLoading";
import { ConversationContext } from "./ConversationContext";
import { BackgroundPanel } from "./BackgroundPanel";
import { backgroundWork } from "./background-work";
import { toastAnchor } from "./toast-anchors";
import { ComputerChats, ProjectRows, ProjectSection, SessionList } from "./ProjectGroup";
import { SessionActions, renameSession } from "./SessionActions";
import { ResizeHandle } from "./ResizeHandle";
import { createOrdering, createOrderDropTarget } from "./Sortable";
import { MenuButton, type MenuAction } from "./MenuButton";
import { UniversalSearch, archivedSearchRequested, type ArchivedScope } from "./UniversalSearch";
import { Wordmark } from "./brand";
import { matchesShortcut, shortcutLabel, type SearchEntry } from "./search";
import { modeCopy, offeredModes } from "./approval-mode";
import { registerCyclePanel, stepFocusedPanel, tabButton } from "./tab-cycle";
import { keyPlace, overlayOpen, stepIndex, stepsChats } from "./keyboard-nav";
import { agentReveal, awayChats, type BrowserStage } from "./browser-stage";
import { SelectionBar } from "./SelectionBar";
import { selection } from "./sidebar-selection";
import { createWorktree, resetWorktree, removeWorktree } from "./worktrees";
import { parseDeepLink } from "../deep-link";
import { localServer } from "../servers";
import { exportLogs } from "./Diagnostics";
import { SideChatPanel, closeFrontSideThread, openSideChat, sideChatEmptied, sideChatFocused, sideChatRequested } from "./SideChat";
import { CHAT_MIN, DOCK_MIN, SIDE_MAX, SIDE_MIN, SIDE_WIDTH, sidePlace } from "./side-chat";
type Dock = "files" | "diff" | "context" | "browser" | "running";

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
  {
    name: "Side chat",
    icon: "side-chat",
    detail: "Ask about this chat without adding to it",
    shortcut: "sideChat",
  },
];
const dockNames: Record<Dock, string> = {
  browser: "Browser",
  files: "Files",
  diff: "Review",
  context: "Context",
  running: "Still running",
};
const dockIcons: Record<Dock, IconName> = {
  browser: "web",
  files: "folder",
  diff: "diff",
  context: "chart",
  running: "running",
};
const Explorer = lazy(() =>
  import("./Explorer").then((module) => ({ default: module.Explorer })),
);
import { Review } from "./Review";
import { chatTitle } from "./chat-title";
import { tabOrderScope } from "./chat-tabs";
import { revealInStrip } from "./reveal";
import { chatRowSelector, rovingRows, rowAfter } from "./roving-rows";
const TerminalPanel = lazy(() =>
  import("./Terminal").then((module) => ({ default: module.TerminalPanel })),
);
const MemoryDialog = lazy(() => import("./MemoryDialog").then((module) => ({default: module.MemoryDialog})));
const Settings = lazy(() =>
  import("./Settings").then((module) => ({ default: module.Settings })),
);
/** This computer, a remote server, Haarness Cloud or another of your
 * computers, each with its own picture. */
const serverIcon = (server: { url?: string; cloud?: boolean; host?: string }): IconName =>
  server.cloud ? "cloud" : server.url ? "server" : server.host ? "host" : "computer";
export function App() {
  const app = useApp();
  const { state, set } = app;
  /** Whether the switcher showed when Desktop last knew, kept while the
   * account is read at launch so the sidebar does not shift. */
  const [switcherBefore, setSwitcherBefore] = createSignal(false);
  /** Whether there may be another server to switch to: one added, the
   * server shown is not this computer, or signed in, where your other
   * computers are listed (read only when the switcher opens). */
  const serverChoices = () =>
    app.servers().length > 1 ||
    computers.items.length > 0 ||
    state.activeServer !== localServer.id ||
    (accountKnown() ? signedIn() : switcherBefore());
  /** Your other computers in the server switcher, once the account has
   * listed them; the one open now stays listed while it is read again. */
  const computerItems = (): MenuAction[] => {
    const active = app.currentServer().host;
    const listed = computers.items.some((item) => item.id === active)
      ? computers.items
      : [
          ...(active && computers.status !== "ready"
            ? [{ id: active, name: app.currentServer().label, connected: true }]
            : []),
          ...computers.items,
        ];
    // While the first list is read, signed in, one quiet row holds the
    // place, so the menu does not grow under the pointer.
    if (!listed.length && signedIn() && ["idle", "loading"].includes(computers.status))
      return [
        {
          label: t("Looking for your computers…"),
          detail: "Haarness Cloud",
          icon: "host",
          disabled: true,
          divider: true,
          heading: t("Your computers"),
        },
      ];
    return listed.map((computer, index) => ({
      label: computer.name,
      detail: computer.connected
        ? platformName("platform" in computer ? computer.platform : null) || t("Connected")
        : lastSeen(computer),
      icon: computer.id === active ? "check" : "host",
      // An away computer cannot be opened until it is back.
      disabled: !computer.connected && computer.id !== active,
      divider: index === 0,
      heading: index === 0 ? t("Your computers") : undefined,
      run: () => void app.run(() => app.openComputer(computer)),
    }));
  };
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
  /** Another chat of the open project reading its history: the conversation
   * and its message box stay on screen, and Session shows the loading mark
   * over the transcript, so a switch never blanks the window. */
  const chatLoading = () =>
    !!state.loadingChat && state.ready && !state.openingProject && !state.openingChat;
  const [palette, setPalette] = createSignal(false);
  /** Search's scope when the sidebar opened it on archived chats. */
  const [paletteArchived, setPaletteArchived] = createSignal<ArchivedScope>();
  createEffect(
    on(
      archivedSearchRequested,
      (request) => {
        if (!request) return;
        setPaletteArchived({ project: request.project, location: request.location });
        setPalette(true);
      },
      { defer: true },
    ),
  );
  const [filesVisited, setFilesVisited] = createSignal(false);
  const [dock, setDock] = createSignal<Dock | "tools" | null>(null);
  const [dockTabs, setDockTabs] = createSignal<Dock[]>([]);
  /** The open chat's work in the background: the title bar's count and Still running. */
  const work = backgroundWork();
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
  /** The side chat: wanted on screen, how wide, and whether it is in front
   * when it and the side panel cannot both show. */
  const [sideOpen, setSideOpen] = createSignal(false);
  const [sideWidth, setSideWidth] = createSignal(SIDE_WIDTH);
  const [sideFront, setSideFront] = createSignal(true);
  const [workbenchWidth, setWorkbenchWidth] = createSignal(1050);
  const [windowWidth, setWindowWidth] = createSignal(window.innerWidth);
  const followWindow = () => setWindowWidth(window.innerWidth);
  window.addEventListener("resize", followWindow);
  onCleanup(() => window.removeEventListener("resize", followWindow));
  /** The room the conversation and the side panel need side by side. */
  const besideRoom = CHAT_MIN + 1 + DOCK_MIN;
  /** The side panel lies over the conversation when the workbench has no
   * room for both (shell.css, `.dock-over`). */
  const dockCovers = () => workbenchWidth() < besideRoom;
  /** Shown again by the person while the sidebar made room beside the chat;
   * it then stays until the side panel and the side chat are closed. */
  const [keepSidebar, setKeepSidebar] = createSignal(false);
  /** The room the conversation needs with what stands beside it: the side
   * panel (the side chat then shares its place), or else the side chat. */
  const roomNeeded = () =>
    dockVisible()
      ? dockExpanded() ? 0 : besideRoom
      : sideWanted() ? CHAT_MIN + 1 + SIDE_MIN : 0;
  /** The sidebar steps aside, before the side panel or the side chat covers
   * the chat, when that alone gives them room side by side. */
  const sidebarYields = () => {
    const need = roomNeeded();
    return (
      sidebarVisible() &&
      !keepSidebar() &&
      need > 0 &&
      windowWidth() - sidebarWidth() - 1 < need &&
      windowWidth() >= need
    );
  };
  const sidebarShown = () => sidebarVisible() && !sidebarYields();
  createEffect(() => {
    if (!roomNeeded()) setKeepSidebar(false);
  });
  function showSidebar() {
    setSidebarVisible(true);
    if (roomNeeded()) setKeepSidebar(true);
  }
  function toggleSidebar() {
    if (sidebarShown()) setSidebarVisible(false);
    else showSidebar();
  }
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
  const dockVisible = () =>
    !!state.connection && !!dock() && state.panel === "chat";
  /** Another computer's pages would all load through Haarness Cloud, and
   * its relayed socket ends with each sign-in: the Browser is for projects on
   * this computer and your servers. */
  const browserHere = () => !state.connection?.remoteHost;
  createEffect(() => {
    if (!browserHere() && dock() === "browser")
      setDock(dockTabs().filter((tab) => tab !== "browser").at(-1) ?? "tools");
  });
  // The new-chat screen has nothing to ask about, so the side chat waits for a chat.
  const sideWanted = () => !!state.connection && !!state.selected && sideOpen() && state.panel === "chat";
  /** Where the side chat stands beside the conversation and the side panel (see `sidePlace`). */
  const side = createMemo(() =>
    sidePlace({
      width: workbenchWidth(),
      wanted: sideWidth(),
      // Without room beside the chat the side panel lies over everything, expanded or not.
      dock: !dockVisible() ? "none" : dockCovers() ? "overlay" : dockExpanded() ? "expanded" : "column",
    }),
  );
  /** Both show side by side, or the one in front does. */
  const sideOnScreen = () => sideWanted() && (side().place === "column" || !dockVisible() || sideFront());
  const dockOnScreen = () => dockVisible() && (!sideWanted() || side().place === "column" || !sideFront());
  const sideRoom = () => (sideOnScreen() && side().place === "column" ? side().width + 1 : 0);
  const dockMax = () => Math.max(360, (workbench?.clientWidth ?? 1050) - 480 - sideRoom());
  const sideMax = () =>
    Math.max(SIDE_MIN, Math.min(SIDE_MAX, workbenchWidth() - 481 - (dockOnScreen() && !dockExpanded() ? 361 : 0)));
  /** `forward`: the person asked for it, so it comes in front of the side chat where both cannot show. */
  function openDock(value: Dock, forward = true) {
    if (value === "browser" && !browserHere()) return;
    if (forward) setSideFront(false);
    setDock(value);
    if (!dockTabs().includes(value)) setDockTabs((tabs) => [...tabs, value]);
    if (value === "files") setFilesVisited(true);
    set("panel", "chat");
  }
  createEffect(() => {
    const request = state.fileRequest;
    if ((request?.preview || request?.link) && request.connection === state.connection?.id && (!request.thread || request.thread === state.selected)) openDock("files", !request.automatic);
  });
  /** The agent took the Browser's tab to another page while another panel held the side panel. */
  const [browserMoved, setBrowserMoved] = createSignal(false);
  /** When the person last asked the Browser to pick an element; it opens to take the request. */
  const [pickRequest, setPickRequest] = createSignal(0);
  function pickElement() {
    if (!browserHere()) return;
    openDock("browser");
    setPickRequest(Date.now());
  }
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
  /** Where the Browser stands: on screen, the side panel hidden or on its launcher, a tab of its own. */
  const browserStage = (): BrowserStage => ({
    chat: JSON.stringify([state.connection?.id ?? "", state.selected ?? ""]),
    shown: dockOnScreen() && dock() === "browser",
    hidden: dock() === null,
    launcher: dock() === "tools",
    tab: dockTabs().includes("browser"),
  });
  /**
   * The chats whose Browser the person put away: hid the side panel or closed
   * the Browser's tab while it showed, and has not shown it since.
   */
  const [awayIn, setAwayIn] = createSignal<ReadonlySet<string>>(new Set());
  createEffect(
    on(browserStage, (next, prev) => setAwayIn((away) => awayChats(away, prev, next))),
  );
  const browserAway = () => awayIn().has(browserStage().chat);
  /**
   * The agent opened or focused a tab, or took the tab the Browser shows to
   * another page. A closed side panel (or one showing only the panel
   * launcher) opens on the Browser, unless the person put the Browser away;
   * otherwise the Browser keeps its place, or gets one back, with a dot that
   * goes once the person looks. The agent never takes the side panel from
   * another panel or leaves another page for the chat.
   */
  function revealBrowser() {
    if (!browserHere()) return;
    const step = agentReveal(browserStage(), browserAway());
    if (step === "none") return;
    if (!dockTabs().includes("browser"))
      setDockTabs((tabs) => [...tabs, "browser"]);
    if (step === "show") setDock("browser");
    else setBrowserMoved(true);
  }
  /** Shows the side panel: on the Browser when the agent left it a dot. */
  function showDock(fallback: Dock | "tools") {
    setSideFront(false);
    setDock(browserMoved() ? "browser" : fallback);
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
  /** Brings the open chat's side chat on screen, its box focused; `fresh` starts another thread. */
  function openSide(fresh = false) {
    if (!state.connection) return;
    if (!state.selected) {
      app.notify(t("Send a first message, then ask side questions with /btw."));
      return;
    }
    void openSideChat({ connection: state.connection.id, chat: state.selected, fresh });
  }
  /** The message box takes the keyboard, unless something covers it. */
  const focusComposer = () => {
    const box = document.querySelector<HTMLElement>("[data-composer-input]");
    if (box?.checkVisibility({ visibilityProperty: true })) box.focus();
  };
  /** Puts the side chat away; the keyboard goes back to the message box. */
  function hideSide() {
    setSideOpen(false);
    focusComposer();
  }
  /** Esc in an idle side chat box: back to the message box, putting the
   * side chat away when it covers the conversation. */
  function leaveSide() {
    if (side().place === "over") hideSide();
    else focusComposer();
  }
  /**
   * The side chat's key (heard by the main process, see `before-input-event`
   * there, or the View menu): brings it on screen with its box focused, and
   * from that box puts it away again, back to the message box. Never behind a dialog.
   */
  function toggleSide() {
    if (state.settings || document.querySelector("dialog[open]")) return;
    if (sideOnScreen() && sideChatFocused()) return hideSide();
    openSide();
  }
  createEffect(
    on(
      sideChatRequested,
      () => {
        setSideFront(true);
        setSideOpen(true);
        set("panel", "chat");
      },
      { defer: true },
    ),
  );
  createEffect(on(sideChatEmptied, hideSide, { defer: true }));
  onMount(() => {
    const observer = new ResizeObserver(() => setWorkbenchWidth(workbench.clientWidth));
    observer.observe(workbench);
    onCleanup(() => observer.disconnect());
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
      if (typeof raw.sideChatWidth === "number" && Number.isFinite(raw.sideChatWidth))
        setSideWidth(clamp(raw.sideChatWidth, SIDE_MIN, SIDE_MAX));
      if (raw.sideChat === true) setSideOpen(true);
      if (typeof raw.serverSwitcher === "boolean") setSwitcherBefore(raw.serverSwitcher);
      if (typeof raw.sideChatFront === "boolean") setSideFront(raw.sideChatFront);
      // Restored as it was: neither comes in front of the other.
      if (["files", "diff", "context", "browser", "running"].includes(String(raw.dock)))
        openDock(raw.dock as Dock, false);
      else if (raw.dock === "tools") setDock("tools");
    }
    setLayoutReady(true);
  });
  // The layout is written 200 ms after it last changed (a drag changes it on
  // every frame), and at once when the page goes away: a panel opened or a
  // split dragged just before a reload, or before the window closes, is kept.
  let unsavedLayout: Record<string, unknown> | undefined;
  const saveLayout = () => {
    const layout = unsavedLayout;
    unsavedLayout = undefined;
    if (layout) void window.haarness.storage("layout:v2", layout);
  };
  createEffect(() => {
    if (!layoutReady()) return;
    unsavedLayout = {
      sidebarVisible: sidebarVisible(),
      sidebarWidth: sidebarWidth(),
      dockWidth: dockWidth(),
      terminalHeight: terminalHeight(),
      dock: dock(),
      sidebarLayout: sidebarLayout(),
      sideChat: sideOpen(),
      sideChatWidth: sideWidth(),
      sideChatFront: sideFront(),
      serverSwitcher: accountKnown() ? serverChoices() : switcherBefore(),
    };
    const timer = setTimeout(saveLayout, 200);
    onCleanup(() => clearTimeout(timer));
  });
  window.addEventListener("pagehide", saveLayout);
  onCleanup(() => {
    window.removeEventListener("pagehide", saveLayout);
    saveLayout();
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
    () => tabOrderScope(state.connection?.id ?? ""),
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
      // The chat loads (its message box hidden meanwhile, which drops the
      // focus) before the keyboard goes back: to the message box when the
      // step started there, else to the chat's tab.
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
      tabs: () => (dockOnScreen() ? panelTabs.ordered() : []),
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
    ...document.querySelectorAll<HTMLButtonElement>(".sidebar " + chatRowSelector),
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
    showSidebar();
    const project = app.currentProject();
    if (project) await app.expandProject(project.id, true);
    const location = app.project();
    if (location && app.state.expandedLocations[location.id] === false)
      await app.expandLocation(location.id, true);
    // The rows of a project just expanded are drawn by then.
    await new Promise((resolve) => setTimeout(resolve));
    const row =
      chatRows().find((row) => row.getAttribute("aria-current") === "true") ??
      chatRows()[0] ??
      document.querySelector<HTMLElement>(".sidebar-lists button");
    // Asked for by a key or a command, the row shows its keyboard ring even
    // when a click came last (Search picked with the pointer).
    document.documentElement.removeAttribute("data-pointer-focus");
    row?.focus({ focusVisible: true } as FocusOptions);
    row?.scrollIntoView({ block: "nearest" });
  }
  /** Whether a chat row has the focus only from a click (`rovingRows`). */
  let rowClicked: (row: Element) => boolean = () => false;
  /** Delete, or Command+Backspace (Control+Backspace elsewhere), on a chat
   * row the keyboard is on archives it, as its archive button does (an open
   * chat stays on screen with its Restore line), and leaves the keyboard on
   * the row below. A row that only took the focus from a click is not on
   * the keyboard, so a key meant for a message never archives it. Select
   * chats has its own Archive. */
  function archiveRow(event: KeyboardEvent) {
    if (event.altKey || event.shiftKey || selection()) return false;
    const mod = state.platform === "darwin" ? event.metaKey : event.ctrlKey;
    const other = state.platform === "darwin" ? event.ctrlKey : event.metaKey;
    const key =
      (event.key === "Delete" && !mod) || (event.key === "Backspace" && mod);
    if (!key || other) return false;
    const rows = chatRows();
    const row = document.activeElement as HTMLButtonElement;
    const archive = rows.includes(row) && !rowClicked(row)
      ? row.parentElement?.querySelector<HTMLButtonElement>(":scope > .icon-button")
      : undefined;
    if (!archive) return false;
    const next = rowAfter(rows, row);
    archive.click();
    let frames = 120;
    const land = () => {
      if (row.isConnected) {
        if (frames-- > 0) requestAnimationFrame(land);
        return;
      }
      const focused = document.activeElement;
      if (!focused || focused === document.body) next?.focus();
    };
    requestAnimationFrame(land);
    return true;
  }
  /** On a chat row that has the keyboard, ↑ and ↓ move it to the row above
   * or below, and Home and End to the first or last, as in a list; Enter
   * opens the chat. Alt with an arrow still moves the row itself (Sortable
   * stops that key first). */
  function walkRows(event: KeyboardEvent) {
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return false;
    if (!["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return false;
    const rows = chatRows();
    const at = rows.indexOf(document.activeElement as HTMLButtonElement);
    if (at < 0) return false;
    const target =
      rows[
        event.key === "Home"
          ? 0
          : event.key === "End"
            ? rows.length - 1
            : Math.min(rows.length - 1, Math.max(0, at + (event.key === "ArrowDown" ? 1 : -1)))
      ];
    target.focus();
    target.scrollIntoView({ block: "nearest" });
    return true;
  }
  onMount(() => {
    const keys = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing) return;
      if (walkRows(event) || archiveRow(event)) return void event.preventDefault();
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
      if (focusing ? place === "overlay" : !stepsChats(place, state.platform === "darwin"))
        return;
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
      {key:"browser",group:"Project panels",name:t("Open browser"),keywords:"web website Chromium tabs",icon:"web",hint:hint("browser", "Mod+Shift+B"),disabled:!connected||!browserHere(),action:()=>openDock("browser")},
      {key:"pick",group:"Project panels",name:t("Select part of the page"),keywords:"pick an element inspect select element page point browser chip",icon:"pick",hint:hint("pick"),disabled:!connected||!browserHere(),action:pickElement},
      {key:"conversation:side-chat",group:"Project panels",name:t("Open side chat"),keywords:"btw side question aside ask thread",icon:"side-chat",hint:hint("sideChat", "Mod+Shift+J"),disabled:!connected||!state.selected,action:()=>openSide()},
      {key:"side-chat-new",group:"Project panels",name:t("New side chat"),keywords:"btw side question aside ask thread another",icon:"side-chat",disabled:!connected||!state.selected,action:()=>openSide(true)},
      {key:"memory",group:"Chat actions",name:t("Manage memory"),keywords:"project memory index",icon:"layers",disabled:!connected,action:()=>set("memoryOpen",true)},
      // One entry for each mode the chat is not in: Yolo only where the
      // server offers it.
      ...offeredModes(app.yoloOffered(), app.approvalMode())
        .filter((mode) => mode !== app.approvalMode())
        .map(
          (mode): SearchEntry => ({
            key: "approvals:" + mode,
            group: "Chat actions",
            name:
              mode === "review"
                ? t("Switch approvals to Review")
                : mode === "auto"
                  ? t("Switch approvals to Auto")
                  : t("Switch approvals to Yolo"),
            detail: t(modeCopy[mode].detail),
            keywords: "permissions approval approvals mode auto review yolo allow once " + mode,
            icon: modeCopy[mode].icon,
            disabled: !connected,
            action: () => app.setApprovalMode(mode),
          }),
        ),
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
        name: sidebarShown() ? t("Hide sidebar") : t("Show sidebar"),
        icon: "sidebar",
        action: toggleSidebar,
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
        name: dockOnScreen() ? t("Hide side panel") : t("Show side panel"),
        icon: "panel",
        disabled: !connected,
        action: () => {
          set("panel", "chat");
          if (dockOnScreen()) setDock(null);
          else showDock("tools");
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
      // A support tool: listed after the everyday commands.
      {key:"export-logs",group:"Commands",name:t("Export logs…"),keywords:"logs diagnostics zip support bug report share",icon:"download",action:()=>void app.run(exportLogs)},
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
    else if (value === "pick" && state.connection) pickElement();
    else if (value === "export-logs") void app.run(exportLogs);
    else if (value === "side-chat") toggleSide();
    else if (value === "side-chat-close" && state.connection && state.selected)
      closeFrontSideThread(state.connection.id, state.selected);
    else if (value === "terminal") set("terminal", (v) => !v);
    else if (value === "files" || value === "diff") set("panel", value);
  }
  function openPalette() {
    if (!document.querySelector("dialog[open]:not(.settings-dialog):not(.palette-dialog)")) {
      setPaletteArchived(undefined);
      setPalette((value) => !value);
    }
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
      // Normally the main process hears the side chat's key first and the page
      // never does; keys sent to the page directly (tests) arrive here.
      if (matchesShortcut(event, state.preferences.shortcuts.sideChat ?? "", state.platform)) {
        event.preventDefault();
        toggleSide();
        return;
      }
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
      data-fullscreen={state.fullScreen ? "" : undefined}
      style={{
        "--sidebar-width": sidebarShown() ? sidebarWidth() + "px" : "0px",
        "--dock-width": dockWidth() + "px",
        "--terminal-height": terminalHeight() + "px",
      }}
    >
      <header class="titlebar">
        <div class="window-toolbar">
          <Button
            icon="sidebar"
            label={sidebarShown() ? t("Hide sidebar") : t("Show sidebar")}
            active={sidebarShown()}
            onClick={toggleSidebar}
          />
        </div>
        <div class="titlebar-session">
          {/* Nothing before a project opens: the sidebar's wordmark already
              names the app. */}
          <Show when={app.composerShown()}>
            <Icon name={app.project()?.chatHome ? "computer" : "folder"} size="sm" />
            <span class="titlebar-caption">
              {app.project()?.chatHome
                ? t("This computer")
                : (app.currentProject()?.label ?? app.project()?.label)}
            </span>
            <Show when={isWorktree(app.project())}>
              <span class="checkout-badge" title={app.project()?.workspace}>
                <Icon name="branch" size="xs" />
                {/* A bare text node in a flex row cannot take the ellipsis. */}
                <span>{t(locationName(app.project()))}</span>
              </span>
            </Show>
            <Show when={state.connection?.remote}>
              <Show
                when={state.connection?.remoteHost}
                fallback={
                  <span class="badge">{app.currentServer().cloud ? t("Cloud workspace") : t("Remote")}</span>
                }
              >
                {/* Where prompts, tools and terminals run, on every screen. */}
                <span class="badge host-badge" title={app.currentServer().label}>
                  <Icon name="host" size="xs" />
                  <span>{t("on {computer}", { computer: app.currentServer().label })}</span>
                </span>
              </Show>
            </Show>
          </Show>
        </div>
        <div class="titlebar-actions">
          <Show when={state.connection}>
            {/* The open chat's work in the background, while there is some;
                it opens Still running. */}
            <Show when={work.running()}>
              <button
                type="button"
                class="background-pill"
                classList={{ active: dockOnScreen() && dock() === "running" }}
                aria-pressed={dockOnScreen() && dock() === "running"}
                title={t("Still running")}
                onClick={() =>
                  dockOnScreen() && dock() === "running" ? setDock(null) : openDock("running")
                }
              >
                <Icon name="running" size="sm" />
                <span>{t("{count} in background", { count: formatNumber(work.running()) })}</span>
              </button>
            </Show>
            {/* This whole computer, reachable from a phone or any browser
                signed in to the same account; /remote-control shares one
                chat. Absent while the window shows another computer. */}
            <Show when={!state.connection?.remote}>
              <RemoteAccessButton />
            </Show>
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
              label={dockOnScreen() ? t("Hide side panel") : t("Show side panel")}
              disabled={!state.connection}
              active={dockOnScreen()}
              dot={!dockOnScreen() && browserMoved()}
              description={
                !dockOnScreen() && browserMoved()
                  ? t("The agent opened another page")
                  : undefined
              }
              onClick={() =>
                dockOnScreen()
                  ? setDock(null)
                  : showDock(dockTabs().at(-1) ?? "tools")
              }
            />
          </Show>
        </div>
      </header>
      <div class="app-body">
        <aside
          class="sidebar"
          style={{ display: sidebarShown() ? "flex" : "none" }}
        >
          <div class="sidebar-brand">
            <Wordmark class="wordmark" />
          </div>
          {/* With this computer alone there is nothing to switch to, and its
              chats have their own section; Settings → Servers adds more. */}
          <Show when={serverChoices()}>
            <div class="server-switcher">
              <MenuButton
                label={t("Switch server")}
                class="server-switcher-button"
                matchWidth
                onOpen={() => void loadComputers()}
                content={
                  <>
                    <Icon name={serverIcon(app.currentServer())} size="md" />
                    <span>{t(app.currentServer().label)}</span>
                    <Icon name="down" size="xs" />
                  </>
                }
                items={[
                  ...app
                    .servers()
                    .filter((server) => !server.host)
                    .map((server) => ({
                      label: t(server.label),
                      detail:
                        server.url ??
                        t(server.cloud ? "Cloud workspaces" : "Local projects"),
                      icon: (server.id === state.activeServer
                        ? "check"
                        : serverIcon(server)) as IconName,
                      run: () => void app.run(() => app.switchServer(server.id)),
                    })),
                  ...computerItems(),
                  {
                    label: t("Manage servers"),
                    icon: "settings",
                    divider: computerItems().length > 0,
                    run: () => {
                      void app.openSettings("servers");
                    },
                  },
                ]}
              />
            </div>
          </Show>
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
            <button class="nav-row" aria-label={t("Search")} data-search-button onClick={openPalette}>
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
          <div
            class="sidebar-lists"
            ref={(element) =>
              onMount(() => {
                const rows = rovingRows(element);
                rowClicked = rows.clicked;
                onCleanup(rows.dispose);
              })
            }
            classList={{ "is-scrolling": sidebarScrolling() }}
            onScroll={showSidebarScrollbar}
          >
            <Show when={state.activeServer === localServer.id}>
              <ComputerChats />
            </Show>
            <div
              class="section-label sidebar-heading"
              classList={{ "hover-actions": projects.ordered().length > 0 }}
            >
              <span>{t("Projects")}</span>
              <div class="section-label-actions">
                <Button
                  icon="plus"
                  label={t("Open another project")}
                  onClick={() => app.run(open)}
                />
                {/* With no project yet there is nothing to organize. */}
                <Show when={projects.ordered().length}>
                  <MenuButton
                    icon="more"
                    label={t("Project list options")}
                    items={[
                      {
                        heading: t("Organize sidebar"),
                        label: t("By project"),
                        checked: sidebarLayout() === "project",
                        run: () => setSidebarLayout("project"),
                      },
                      {
                        label: t("In one list"),
                        checked: sidebarLayout() === "flat",
                        run: () => setSidebarLayout("flat"),
                      },
                    ]}
                  />
                </Show>
              </div>
            </div>
            <div class="project-list" {...unfiledDrop}>
              {/* With no project yet, the + above (and the message box's folder
                  chip) is the way in; the list stays empty rather than repeat it. */}
              <Show when={projects.ordered().length}>
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
            <UpdateRow />
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
        <Show when={sidebarShown()}>
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
              "has-dock": dockOnScreen(),
              "dock-over": dockOnScreen() && dockCovers(),
              "dock-expanded": dockOnScreen() && dockExpanded(),
              "side-over": sideOnScreen() && side().place === "over",
              "side-column": sideOnScreen() && side().place === "column",
            }}
            style={{ "--side-chat-room": sideRoom() + "px" }}
          >
            <div class="chat-pane" data-cycle-panel="chats">
              {/* The bar stays while another chat of this project loads its
                  history (a tab chosen), so the tabs do not blink. */}
              <Show
                when={
                  state.panel === "chat" &&
                  !!state.selected &&
                  (!loading() || (state.ready && !state.openingProject && !state.openingChat))
                }
              >
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
                              preview: id === state.preview,
                            }}
                          >
                            <button
                              role="tab"
                              data-cycle-tab={id}
                              aria-selected={id === state.selected}
                              aria-description={
                                id === state.preview
                                  ? t("Preview tab. The next chat you open replaces it. Press Enter or double-click to keep it open.")
                                  : undefined
                              }
                              title={tabTitle(id)}
                              ref={(element) =>
                                createEffect(() => {
                                  if (id === state.selected && state.panel === "chat")
                                    revealInStrip(element.parentElement!);
                                })
                              }
                              onClick={(event) => {
                                // Enter or Space on the preview's own tab keeps it.
                                if (event.detail === 0 && id === state.selected) app.keepTab(id);
                                void app.run(() => app.choose(id));
                              }}
                              onDblClick={() => app.keepTab(id)}
                            >
                              <Show when={running(id)}>
                                <span class="session-status running" aria-hidden="true" />
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
                {/* This computer's new chat shows its message box while it
                    opens, never a loading screen, and so does another chat of
                    the open project while its history loads. */}
                <Show when={state.panel === "chat" && loading() && !app.homeOpening() && !chatLoading()}>
                  <ChatLoading
                    label={
                      state.openingChat || state.loadingChat
                        ? t("Loading chat…")
                        : !state.openingProject
                          ? t("Loading…")
                          : state.openingProject === app.chatHomeLocation().id
                            ? t("Opening This computer…")
                            : t("Opening project…")
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
                      state.panel === "chat" &&
                      (app.homeOpening() ||
                        (app.composerShown() && (!loading() || chatLoading())))
                        ? "flex"
                        : "none",
                  }}
                >
                  <Session />
                </div>
              </div>
            </div>
            <Show when={sideOnScreen()}>
              <Show when={side().place !== "over"}>
                <ResizeHandle
                  label={t("Resize side chat")}
                  value={side().width}
                  min={SIDE_MIN}
                  max={sideMax()}
                  onDelta={(delta) => setSideWidth(clamp(side().width - delta, SIDE_MIN, sideMax()))}
                  onReset={() => setSideWidth(SIDE_WIDTH)}
                />
              </Show>
              <section
                class="side-chat-column"
                classList={{ over: side().place === "over" }}
                style={{ width: side().place === "over" ? undefined : side().width + "px" }}
              >
                <SideChatPanel onHide={hideSide} onEscape={leaveSide} />
              </section>
            </Show>
            <Show when={dockOnScreen() && !dockExpanded()}>
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
              style={{ display: dockOnScreen() ? "flex" : "none" }}
            >
              <div class="dock-tabs">
                {/* Laid over the chat, the strip leads with the way back to
                    it; the side panel button brings the panel back. */}
                <Show when={dockCovers()}>
                  <button
                    class="dock-return"
                    aria-label={t("Return to chat")}
                    onClick={() => {
                      setDock(null);
                      focusComposer();
                    }}
                  >
                    <Icon name="chat" size="sm" />
                    {t("Chat")}
                  </button>
                  <span class="dock-tabs-rule" aria-hidden="true" />
                </Show>
                <For each={(panelTabs.ordered() as Dock[]).filter((tab) => tab !== "browser" || browserHere())}>
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
                        aria-label={t("Show {name} panel", { name: t(dockNames[tab]) })}
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
                        label={t("Close {name} panel", { name: t(dockNames[tab]) })}
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
                  <For each={panelChoices.filter((item) => item.shortcut !== "browser" || browserHere())}>
                    {(item) => (
                      <button
                        onClick={() =>
                          app.run(async () => {
                            if (item.shortcut === "terminal")
                              await openTerminal();
                            else if (item.shortcut === "sideChat") openSide();
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
              <Show when={state.connection && dock() === "running"}>
                <BackgroundPanel work={work} />
              </Show>
              <Show when={browserHere()}>
                <BrowserPanel visible={dockOnScreen() && dock() === "browser"} onOpen={(agent) => (agent ? revealBrowser() : openDock("browser"))} onReveal={revealBrowser} pickRequest={pickRequest()} />
              </Show>
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
            <div class="terminal-dock" ref={toastAnchor("terminal")}>
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
      <ImageViewer />
      <ServerConnect />
      <Show when={palette()}>
        <UniversalSearch archived={paletteArchived()} close={() => setPalette(false)} />
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
