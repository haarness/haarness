import { previewInfo } from "./preview-actions";
import { createAttachmentDrafts } from "./attachment-drafts";
import { applyLanguage } from "./i18n";
import {
  autoApproval,
  approveAndEnable,
  canAutoApprove,
  createAutoApprover,
  readApprovalModes,
  type ApprovalModes,
} from "./approval-mode";
import { conversationRoot } from "../thread-kinds";
import {
  addContext,
  contextKey,
  promptWithContext,
  readContext,
  type PromptContext,
} from "./prompt-context";
import { readModelRecents, type ModelKey } from "./model-selection";
import {
  createContext,
  batch,
  useContext,
  onCleanup,
  createEffect,
  createMemo,
  createRoot,
  createSignal,
  type ParentProps,
} from "solid-js";
import { createStore, reconcile, unwrap } from "solid-js/store";
import type * as T from "@haarness/sdk";
import {
  defaults,
  type Connection,
  type Preferences,
  type Recent,
  type SavedServer,
  type DesktopEvent,
  type NotificationTarget,
} from "../shared";
import { cloudServers, localServer, serverKey, serverProjects } from "../servers";
import { reduceEvent, type Live } from "./sync";
import { themeColors } from "../themes";
import {
  cleanProjectMeta,
  groupProjects,
  migrateProjectPresentation,
  primaryLocation,
  projectKey,
  readProjectMeta,
  sessionKey,
  type ProjectMeta,
  type ProjectMetaMap,
} from "../projects";
import { createProjectOverview } from "./project-overview";
import { checkAttachments, fileAttachments, type Attachment } from "./media";
import type { SearchEntry } from "./search";
import {
  readOrdering,
  orderedIds,
  moveInOrder,
  type Ordering,
} from "./ordering";
/** Which surface the main column is showing. */
export type Pane = "chat" | "files" | "diff";

interface State extends Live {
  modelRecents: ModelKey[];
  approvalModes: ApprovalModes;
  ordering: Ordering;
  expandedProjects: Record<string, boolean>;
  worktreeViews: Record<string, boolean>;
  expandedLocations: Record<string, boolean>;
  /** Pins, custom names and sections, by project ID. */
  projectMeta: ProjectMetaMap;
  /** Whether the sidebar groups chats under projects or lists them flat. The
   * flat list shows every project's chats, so all indexes stay live for it. */
  sidebarLayout: "project" | "flat";
  connection: Connection | null;
  recent: Recent[];
  servers: SavedServer[];
  activeServer: string;
  remoteProjectRequest: { server: SavedServer; newChat: boolean } | null;
  preferences: Preferences;
  version: string;
  platform: string;
  selected: string;
  tabs: string[];
  items: T.Item[];
  providers: T.ProviderInfo[];
  agents: T.AgentProfile[];
  commands: T.CommandInfo[];
  panel: Pane;
  settings: string | null;
  /** The connect-a-provider dialog, when open. */
  connect: { provider?: string; onboarding?: boolean } | null;
  memoryOpen: boolean;
  settingsTarget: string | null;
  settingsRevision: number;
  searchOpen: boolean;
  fileRequest: {
    connection: string;
    path: string;
    sequence: number;
    mode?: "preview" | "code";
    version?: string;
    thread?: string;
    preview?: boolean;
    action?: "file" | "folder";
  } | null;
  terminal: boolean;
  ready: boolean;
  openingProject: string;
  openingChat: number;
  loadingChat: number;
  projectFailure: {
    input: Parameters<typeof window.haarness.connect>[0];
    message: string;
  } | null;
  error: string;
  permissionErrors: Record<string, Record<string, string>>;
  notice: string;
  connectionStatus: string;
  drafts: Record<string, string>;
  catalogVersion: number;
}
function createApp() {
  const [state, set] = createStore<State>({
    modelRecents: [],
    approvalModes: {},
    ordering: {},
    expandedProjects: {},
    worktreeViews: {},
    expandedLocations: {},
    projectMeta: {},
    sidebarLayout: "project",
    connection: null,
    recent: [],
    servers: [],
    activeServer: "local",
    remoteProjectRequest: null,
    preferences: { ...defaults },
    version: "",
    platform: "",
    selected: "",
    tabs: [],
    items: [],
    threads: [],
    turns: [],
    permissions: [],
    questions: [],
    streams: {},
    tasks: {},
    providers: [],
    agents: [],
    commands: [],
    panel: "chat",
    settings: null,
    connect: null,
    memoryOpen: false,
    settingsTarget: null,
    settingsRevision: 0,
    searchOpen: false,
    fileRequest: null,
    terminal: false,
    ready: false,
    openingProject: "",
    openingChat: 0,
    loadingChat: 0,
    projectFailure: null,
    error: "",
    permissionErrors: {},
    notice: "",
    connectionStatus: "",
    drafts: {},
    catalogVersion: 0,
  });
  let generation = 0,
    selection = 0,
    subscription = "",
    reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  let navigation = 0;
  let hydration = 0,
    connectAttempt = 0,
    reconnectAttempt = 0,
    stopped = false;
  const journals = new Set<T.EventEnvelope[]>();
  let tabHistory: string[] = [];
  let lastCursor = 0,
    lastInstance = "",
    forceSnapshot = true;
  let noticeTimer: ReturnType<typeof setTimeout> | undefined;
  let leaveSettings: () => Promise<boolean> = async () => true;
  let leaveWorkspace: () => Promise<boolean> = async () => true;
  let menuHandler: (data: unknown) => void = () => {};
  const [registrations, setRegistrations] = createSignal<
    Record<string, () => SearchEntry[]>
  >({});
  function registerSearch(id: string, entries: () => SearchEntry[]) {
    setRegistrations((value) => ({ ...value, [id]: entries }));
    onCleanup(() =>
      setRegistrations((value) => {
        if (value[id] !== entries) return value;
        const next = { ...value };
        delete next[id];
        return next;
      }),
    );
  }
  const api = <V,>(method: string, ...args: unknown[]) => {
    if (!state.connection) throw new Error("Open project first");
    return window.haarness.api<V>(state.connection.id, method, args);
  };
  const native = <V,>(input: Record<string, unknown>) =>
    window.haarness.native<V>(state.connection!.id, input);
  function notify(text: string) {
    set("notice", text);
    clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => set("notice", ""), 5000);
  }
  async function run<V>(operation: () => Promise<V>): Promise<V | undefined> {
    set("error", "");
    try {
      return await operation();
    } catch (error) {
      set("error", error instanceof Error ? error.message : String(error));
      return undefined;
    }
  }
  let approvalRevision = 0;
  function permissionError(id: string) {
    return state.permissionErrors[state.connection?.id ?? ""]?.[id];
  }
  function recordPermissionError(
    connection: string,
    id: string,
    error: unknown,
  ) {
    set("permissionErrors", connection, {
      ...state.permissionErrors[connection],
      [id]: error instanceof Error ? error.message : String(error),
    });
  }
  function retainPermissionErrors(
    connection: string,
    pending: T.PendingPermissionInfo[],
  ) {
    const ids = new Set(pending.map((request) => request.permissionId));
    set(
      "permissionErrors",
      connection,
      reconcile(
        Object.fromEntries(
          Object.entries(state.permissionErrors[connection] ?? {}).filter(
            ([id]) => ids.has(id),
          ),
        ),
      ),
    );
  }
  let modelSave = Promise.resolve<unknown>(undefined);
  async function rememberModel(value: ModelKey) {
    const recent = readModelRecents([
      value,
      ...state.modelRecents.filter(
        (item) =>
          item.provider !== value.provider || item.model !== value.model,
      ),
    ]);
    set("modelRecents", recent);
    modelSave = modelSave
      .catch(() => {})
      .then(() => window.haarness.storage("model-recents:v1", recent));
    await modelSave;
  }
  let approvalSave = Promise.resolve<unknown>(undefined);
  function isAutoApproval(
    connection = state.connection?.id ?? "",
    thread = state.selected,
  ) {
    return autoApproval(
      state.approvalModes,
      connection,
      thread,
      connection === state.connection?.id
        ? state.threads
        : (indexes()[connection]?.threads ?? []),
    );
  }
  async function setAutoApproval(
    enabled: boolean,
    connection = state.connection?.id ?? "",
    thread = state.selected,
  ) {
    if (!connection) return;
    approvalRevision++;
    set("approvalModes", connection, {
      ...state.approvalModes[connection],
      [thread || "*"]: enabled,
    });
    const value = JSON.parse(JSON.stringify(state.approvalModes));
    approvalSave = approvalSave
      .catch(() => {})
      .then(() => window.haarness.storage("approval-modes:v1", value));
    await approvalSave;
  }
  async function allowOnceAndAuto(request: T.PendingPermissionInfo) {
    const connection = state.connection?.id;
    if (!connection || !canAutoApprove(request)) return;
    const revision = approvalRevision;
    const selected = state.selected;
    await approveAndEnable({
      reply: () =>
        window.haarness.api(connection, "replyPermission", [
          request.permissionId,
          { decision: "once" },
        ]),
      acknowledge: () =>
        autoRespond.acknowledge(connection, request.permissionId),
      current: () =>
        revision === approvalRevision &&
        connection === state.connection?.id &&
        selected === state.selected,
      enable: () =>
        setAutoApproval(
          true,
          connection,
          selected || request.rootThreadId || request.threadId,
        ),
    });
  }
  const autoRespond = createAutoApprover({
    enabled: (connection, request, threads) =>
      autoApproval(
        state.approvalModes,
        connection,
        request.threadId,
        threads,
        request.rootThreadId,
      ),
    reply: (connection, id) =>
      window.haarness.api(connection, "replyPermission", [
        id,
        { decision: "once" },
      ]),
    failed: async (connection, request, error) => {
      // Another window/client may win the reply race. Only a request that
      // still needs an answer is a failure the user must review.
      try {
        const pending = await window.haarness.api<T.PendingPermissionInfo[]>(
          connection,
          "listPermissions",
          [],
        );
        if (!pending.some((item) => item.permissionId === request.permissionId))
          return;
      } catch {
        /* An unreachable server still needs attention. */
      }
      recordPermissionError(
        connection,
        request.permissionId,
        `Auto approval failed. Review the request manually. ${error instanceof Error ? error.message : String(error)}`,
      );
    },
  });
  let catalogRequest = 0;
  async function catalogs(force = false, knownInfo?: T.ServerInfo) {
    const request = ++catalogRequest;
    const current = generation;
    const values = await Promise.all([
      api<T.ProviderInfo[]>(
        force &&
          state.connection?.info.capabilities.includes("providers.refresh.v1")
          ? "refreshProviders"
          : "providers",
      ),
      api<T.AgentProfile[]>("agents"),
      api<T.CommandInfo[]>("commands"),
      knownInfo ?? api<T.ServerInfo>("info"),
    ]);
    if (current !== generation || request !== catalogRequest) return;
    set("providers", values[0]);
    set("agents", values[1]);
    set("commands", values[2]);
    if (state.connection) set("connection", "info", values[3]);
    set("catalogVersion", (n) => n + 1);
  }
  async function refresh() {
    const current = generation;
    const journal: T.EventEnvelope[] = [];
    journals.add(journal);
    try {
      const values = await Promise.all([
        api<T.Thread[]>("listThreads", { archived: true }),
        api<T.Turn[]>("activeTurns"),
        api<T.PendingPermissionInfo[]>("listPermissions"),
        api<T.PendingQuestionInfo[]>("listQuestions"),
      ]);
      if (current !== generation) return;
      batch(() => {
        set("threads", reconcile(values[0], { key: "id" }));
        set("turns", reconcile(values[1], { key: "id" }));
        set("permissions", values[2]);
        set("questions", values[3]);
        // Replay before painting so an old snapshot cannot briefly hide a live
        // activity or approval, then recreate it from the newer events.
        for (const event of journal)
          if (!event.type.endsWith("_delta") && event.type !== "item.created")
            reduceEvent(state, set, event);
        retainPermissionErrors(state.connection!.id, state.permissions);
      });
    } finally {
      journals.delete(journal);
    }
  }
  async function choose(id: string, panel: State["panel"] = "chat") {
    if (state.selected === id) {
      set("panel", panel);
      return;
    }
    const current = ++selection;
    const connection = state.connection!.id;
    tabHistory = [...tabHistory.filter((tab) => tab !== id), id];
    batch(() => {
      set("loadingChat", current);
      set("selected", id);
      set("items", []);
      set("panel", panel);
      if (!state.tabs.includes(id)) set("tabs", (tabs) => [...tabs, id]);
    });
    try {
      const items = await api<T.Item[]>("listItems", id);
      if (current !== selection) return;
      set(
        "items",
        [
          ...new Map(
            [...items, ...state.items].map((item) => [item.id, item]),
          ).values(),
        ].sort((a, b) => a.sequence - b.sequence),
      );
      await window.haarness.storage("session:" + connection, id);
    } finally {
      if (state.loadingChat === current) set("loadingChat", 0);
    }
  }

  async function closeTab(id: string) {
    set("openingChat", 0);
    set("tabs", (tabs) => tabs.filter((tab) => tab !== id));
    tabHistory = tabHistory.filter((tab) => state.tabs.includes(tab));
    if (state.selected !== id) return;
    const next = tabHistory.at(-1) ?? state.tabs.at(-1);
    if (next) {
      await choose(next);
    } else {
      selection++;
      batch(() => {
        set("loadingChat", 0);
        set("selected", "");
        set("items", []);
      });
      if (state.connection)
        await window.haarness.storage("session:" + state.connection.id, "");
    }
  }
  function reconnect() {
    if (stopped || !state.connection) return;
    set("connectionStatus", "Reconnecting…");
    clearTimeout(reconnectTimer);
    reconnectTimer = setTimeout(
      () => {
        void hydrate().catch((error) => {
          set("error", String(error));
          reconnectAttempt++;
          reconnect();
        });
      },
      Math.min(30000, 500 * 2 ** Math.min(reconnectAttempt, 6)),
    );
  }
  async function hydrate(knownInfo?: T.ServerInfo) {
    const current = generation,
      operation = ++hydration;
    const valid = () =>
      current === generation && operation === hydration && !stopped;
    clearTimeout(reconnectTimer);
    set("connectionStatus", "Connecting…");
    const previous = subscription;
    subscription = "";
    if (previous) await window.haarness.unsubscribe(previous);
    let info: T.ServerInfo;
    try {
      info = knownInfo ?? (await api<T.ServerInfo>("info"));
    } catch (error) {
      if (!valid()) return;
      if (!state.connection) throw error;
      const view = await window.haarness.connect({
        recent: state.connection.id,
      });
      if (!view || !valid()) return;
      set("connection", view);
      info = view.info;
    }
    if (!valid()) return;
    await refresh();
    if (!valid()) return;
    const selected = state.selected;
    if (selected) {
      const items = await api<T.Item[]>("listItems", selected);
      if (!valid()) return;
      if (selected === state.selected) set("items", items);
    }
    const resume = !forceSnapshot && lastInstance === info.serverInstanceId;
    if (!resume) {
      set("streams", {});
      set("tasks", {});
    }
    const cursor = resume ? lastCursor : (info.eventCursor ?? 0);
    lastInstance = info.serverInstanceId;
    lastCursor = cursor;
    forceSnapshot = false;
    const id = await window.haarness.subscribe(state.connection!.id, cursor);
    if (!valid()) {
      await window.haarness.unsubscribe(id);
      return;
    }
    subscription = id;
    reconnectAttempt = 0;
    set("connectionStatus", "Connected");
  }
  createEffect(() => applyLanguage(state.preferences.language));
  createEffect(() => {
    if (state.connection && state.connectionStatus === "Connected")
      autoRespond(state.connection.id, state.permissions, state.threads);
  });
  const projectConnections = new Map<string, Promise<Connection | null>>();
  function projectConnection(id: string) {
    let pending = projectConnections.get(id);
    if (!pending) {
      pending = window.haarness
        .connect({ recent: id })
        .finally(() => projectConnections.delete(id));
      projectConnections.set(id, pending);
    }
    return pending;
  }
  const servers = createMemo(() => [
    localServer,
    ...state.servers,
    ...cloudServers(state.recent),
  ]);
  const currentServer = () =>
    servers().find((item) => item.id === state.activeServer) ?? localServer;
  const visibleLocations = createMemo(() =>
    serverProjects(state.recent, state.activeServer),
  );
  // A custom name replaces the catalog's label everywhere a project is shown.
  const projects = createMemo(() =>
    groupProjects(visibleLocations()).map((project) => {
      const name = state.projectMeta[project.id]?.name;
      return name ? { ...project, label: name } : project;
    }),
  );

  async function clearWorkspace(server: string) {
    ++connectAttempt;
    ++generation;
    ++selection;
    ++hydration;
    activation = undefined;
    clearTimeout(reconnectTimer);
    const previous = subscription;
    subscription = "";
    tabHistory = [];
    batch(() => {
      set("activeServer", server);
      set("connection", null);
      set("openingProject", "");
      set("loadingChat", 0);
      set("projectFailure", null);
      set("selected", "");
      set("items", []);
      set("tabs", []);
      set("threads", []);
      set("turns", []);
      set("permissions", []);
      set("questions", []);
      set("streams", {});
      set("tasks", {});
      set("providers", []);
      set("agents", []);
      set("commands", []);
      set("panel", "chat");
      set("terminal", false);
      set("fileRequest", null);
      set("connectionStatus", "");
      set("ready", true);
    });
    await window.haarness.storage("active-server", server);
    if (previous) await window.haarness.unsubscribe(previous);
  }
  async function switchServer(id: string) {
    if (!servers().some((item) => item.id === id)) return false;
    set("openingChat", 0);
    const attempt = connectAttempt;
    const remembered = await window.haarness.storage("server-project:" + id);
    if (attempt !== connectAttempt) return false;
    const locations = serverProjects(state.recent, id);
    const target = locations.find((item) => item.id === remembered);
    if (target && id === state.activeServer)
      return connect({ recent: target.id });
    if (!(await leaveSettings()) || !(await leaveWorkspace())) return false;
    if (attempt !== connectAttempt) return false;
    const request = connectAttempt + 1;
    await clearWorkspace(id);
    if (request !== connectAttempt) return false;
    if (target) return connect({ recent: target.id });
    return true;
  }
  async function openProject(newChat = false) {
    set("openingChat", 0);
    const server = currentServer();
    if (server.url) {
      set("remoteProjectRequest", { server, newChat });
      return false;
    }
    // Cloud sessions are started and opened from the account dialog.
    if (server.cloud) {
      window.dispatchEvent(new CustomEvent("haarness:account"));
      return false;
    }
    return connect({}, newChat);
  }
  async function forgetProject(id: string) {
    const active = currentProject()?.id === id;
    if (active && (!(await leaveSettings()) || !(await leaveWorkspace())))
      return false;
    await window.haarness.forget(id);
    applyRecent((await window.haarness.boot()).recent);
    if (active) await switchServer(state.activeServer);
    return true;
  }
  async function removeServer(id: string) {
    if (state.activeServer === id && !(await switchServer("local")))
      return false;
    set("servers", await window.haarness.server({ id, action: "remove" }));
    applyRecent((await window.haarness.boot()).recent);
    return true;
  }
  function applyRecent(recent: Recent[]) {
    const presentation = migrateProjectPresentation(
      recent,
      unwrap(state.ordering),
      unwrap(state.expandedProjects),
      unwrap(state.recent),
      unwrap(state.projectMeta),
    );
    const changed =
      JSON.stringify(presentation.ordering) !==
        JSON.stringify(state.ordering) ||
      JSON.stringify(presentation.expanded) !==
        JSON.stringify(state.expandedProjects);
    const metaChanged =
      JSON.stringify(presentation.meta) !== JSON.stringify(state.projectMeta);
    batch(() => {
      set("ordering", presentation.ordering);
      set("expandedProjects", presentation.expanded);
      set("projectMeta", reconcile(presentation.meta));
      set("recent", reconcile(recent, { key: "id" }));
      const current = recent.find((item) => item.id === state.connection?.id);
      if (current && state.connection)
        set("connection", {
          ...state.connection,
          project: current.project,
          checkout: current.checkout,
        });
    });
    if (changed) {
      void window.haarness.storage("ordering:v1", {
        version: 1,
        lists: presentation.ordering,
      });
      void window.haarness.storage("project-expansion:v1", {
        version: 1,
        projects: presentation.expanded,
      });
    }
    if (metaChanged) void persistProjectMeta();
  }
  function persistProjectMeta() {
    return window.haarness.storage("project-presentation:v1", {
      version: 1,
      projects: unwrap(state.projectMeta),
    });
  }
  async function setProjectMeta(map: ProjectMetaMap) {
    set("projectMeta", reconcile(map));
    await persistProjectMeta();
  }
  /** Merges a pin, name or section change; empty values clear the field. */
  async function updateProjectMeta(id: string, patch: ProjectMeta) {
    const map = { ...unwrap(state.projectMeta) };
    const next = cleanProjectMeta({ ...map[id], ...patch });
    if (next) map[id] = next;
    else delete map[id];
    await setProjectMeta(map);
  }
  async function renameSection(from: string, to: string) {
    const name = to.trim();
    if (!name || name === from) return;
    await setProjectMeta(
      Object.fromEntries(
        Object.entries(unwrap(state.projectMeta)).map(([id, meta]) => [
          id,
          meta.section === from ? { ...meta, section: name } : meta,
        ]),
      ),
    );
  }
  /** Files the section's projects under no section; the projects stay. */
  async function removeSection(name: string) {
    const map: ProjectMetaMap = {};
    for (const [id, meta] of Object.entries(unwrap(state.projectMeta))) {
      const next = cleanProjectMeta(
        meta.section === name ? { ...meta, section: "" } : meta,
      );
      if (next) map[id] = next;
    }
    await setProjectMeta(map);
  }
  /** Drops a project beside another one: it joins that project's section and
   * takes the slot before or after it in the shared order. */
  async function fileProjectBeside(
    source: string,
    target: string,
    position: "before" | "after",
  ) {
    const ids = projects().map((item) => item.id);
    if (source === target || !ids.includes(source) || !ids.includes(target)) return;
    const section = state.projectMeta[target]?.section ?? "";
    if ((state.projectMeta[source]?.section ?? "") !== section)
      await updateProjectMeta(source, { section });
    const visible = ordered(
      "list:projects",
      projects().map((item) => item.id),
    );
    const index = visible.filter((id) => id !== source).indexOf(target);
    if (index < 0) return;
    await reorder(
      "list:projects",
      visible,
      source,
      index + (position === "after" ? 1 : 0),
    );
  }
  /** A section drop moves only between sections, appending to the matching lane.
   * Dropping within the current section leaves the project's position alone. */
  async function fileProjectInSection(source: string, section: string) {
    if ((state.projectMeta[source]?.section ?? "") === section) return;
    const visible = ordered("list:projects", projects().map((item) => item.id));
    if (!visible.includes(source)) return;
    const pinned = !!state.projectMeta[source]?.pinned;
    const last = visible.filter((id) =>
      id !== source &&
      (state.projectMeta[id]?.section ?? "") === section &&
      !!state.projectMeta[id]?.pinned === pinned,
    ).at(-1);
    if (last) await fileProjectBeside(source, last, "after");
    else await updateProjectMeta(source, { section });
  }
  /** Archives every independent conversation in a project's available
   * checkouts. The current chat stays open even when it is one of them. */
  async function archiveProjectChats(id: string) {
    const group = projects().find((item) => item.id === id);
    if (!group) return 0;
    let count = 0;
    for (const location of group.locations) {
      if (location.checkout?.exists === false) continue;
      if (!(await projectConnection(location.id))) continue;
      const threads = await window.haarness.api<T.Thread[]>(
        location.id,
        "listThreads",
        [{}],
      );
      for (const thread of threads) {
        if (thread.archived || thread.isSubagent) continue;
        await window.haarness.api(location.id, "updateThread", [
          thread.id,
          { archived: true },
        ]);
        count++;
      }
    }
    if (group.locations.some((item) => item.id === state.connection?.id))
      await refresh();
    return count;
  }
  /** Shows the project folder in the system file manager. */
  async function revealProject(id: string) {
    const group = projects().find((item) => item.id === id);
    if (!group || group.remote) return;
    await window.haarness.reveal(primaryLocation(group).id);
  }
  const [indexes, setIndexes] = createSignal<
    Record<string, ReturnType<typeof createProjectOverview>>
  >({});
  const indexDisposers = new Map<string, () => void>();
  createEffect(() => {
    for (const [id, overview] of Object.entries(indexes())) {
      if (
        id !== state.connection?.id &&
        overview.loaded &&
        !overview.loading &&
        !overview.error
      )
        autoRespond(id, overview.permissions, overview.threads);
    }
  });
  createEffect(() => {
    const locations = state.recent;
    const next = { ...indexes() };
    let changed = false;
    for (const location of locations) {
      if (next[location.id]) continue;
      next[location.id] = createRoot((dispose) => {
        indexDisposers.set(location.id, dispose);
        return createProjectOverview({
          id: () => location.id,
          expanded: () => {
            const current = state.recent.find(
              (item) => item.id === location.id,
            );
            return (
              !!current &&
              current.checkout?.exists !== false &&
              (!!state.expandedProjects[projectKey(current)] ||
                state.sidebarLayout === "flat" ||
                state.searchOpen ||
                Object.values(state.approvalModes[location.id] ?? {}).some(
                  Boolean,
                ))
            );
          },
          active: () => state.connection?.id === location.id,
          snapshot: () =>
            state.connectionStatus === "Connected"
              ? {
                  threads: state.threads,
                  turns: state.turns,
                  permissions: state.permissions,
                  questions: state.questions,
                }
              : undefined,
          bridge: window.haarness,
          connect: projectConnection,
        });
      });
      changed = true;
    }
    for (const id of Object.keys(next)) {
      if (locations.some((item) => item.id === id)) continue;
      indexDisposers.get(id)?.();
      indexDisposers.delete(id);
      delete next[id];
      changed = true;
    }
    if (changed) setIndexes(next);
  });
  onCleanup(() => indexDisposers.forEach((dispose) => dispose()));
  const conversations = (id?: string) =>
    visibleLocations()
      .filter((location) => !id || projectKey(location) === id)
      .flatMap((location) => {
        const live =
          location.id === state.connection?.id &&
          state.connectionStatus === "Connected"
            ? state
            : indexes()[location.id];
        const running = new Set(
          live?.turns.flatMap((turn) => [
            turn.threadId,
            conversationRoot(live.threads, turn.threadId),
          ]),
        );
        return (live?.threads ?? []).map((thread) => ({
          key: sessionKey(location.id, thread.id),
          location,
          thread,
          running: running.has(thread.id),
          waiting: live?.permissions.some(
            (item) => (item.rootThreadId ?? item.threadId) === thread.id,
          )
            ? "Needs approval"
            : live?.questions.some(
                  (item) => (item.rootThreadId ?? item.threadId) === thread.id,
                )
              ? "Needs an answer"
              : "",
        }));
      });
  async function expandProject(id: string, expanded: boolean) {
    if (state.expandedProjects[id] === expanded) return;
    set("expandedProjects", id, expanded);
    await window.haarness.storage("project-expansion:v1", {
      version: 1,
      projects: unwrap(state.expandedProjects),
    });
  }
  async function setWorktreeView(id: string, enabled: boolean) {
    set("worktreeViews", id, enabled);
    await window.haarness.storage(
      "worktree-views:v1",
      unwrap(state.worktreeViews),
    );
    if (enabled) {
      await expandProject(id, true);
      await refreshProjects();
    }
  }
  async function expandLocation(id: string, expanded: boolean) {
    set("expandedLocations", id, expanded);
    await window.haarness.storage(
      "worktree-expansion:v1",
      unwrap(state.expandedLocations),
    );
  }
  let activation: { id?: string; promise: Promise<boolean> } | undefined;
  let newChatAttempt = -1;
  const project = () =>
    state.recent.find((item) => item.id === state.openingProject) ??
    state.recent.find((item) => item.id === state.connection?.id) ??
    state.connection ??
    undefined;
  const currentProject = () =>
    projects().find(
      (item) => item.id === (project() && projectKey(project()!)),
    );
  const newDraftScope = () =>
    state.worktreeViews[currentProject()?.id ?? ""]
      ? (project()?.id ?? "")
      : (currentProject()?.id ?? project()?.id ?? "");
  const draftKey = () =>
    state.selected
      ? sessionKey(state.connection?.id ?? "", state.selected)
      : "new:" + newDraftScope();
  const [attachmentVersion, setAttachmentVersion] = createSignal(0);
  const attachmentDrafts = createAttachmentDrafts(
    (key, value) => window.haarness.attachmentDraft(key, value),
    () => setAttachmentVersion((value) => value + 1),
  );
  const attachmentDraft = (key: string) => { attachmentVersion(); return attachmentDrafts.get(key); };
  const loadAttachmentDraft = (key: string) => attachmentDrafts.load(key);
  function setAttachmentDraft(key: string, value: Attachment[] | ((items: Attachment[]) => Attachment[])) {
    const pending = attachmentDrafts.update(key, value);
    void pending.catch((error) => set("error", "Could not save attachment draft: " + String(error)));
    return pending;
  }
  const addAttachments = (key: string, values: Attachment[]) =>
    setAttachmentDraft(key, (items) => checkAttachments([...items, ...values]));
  async function attachFiles(key: string, files: File[]) {
    await addAttachments(key, await fileAttachments(files));
  }
  createEffect(() => {
    const key = draftKey();
    if (state.ready) void run(() => loadAttachmentDraft(key));
  });
  const [contexts, setContexts] = createSignal<Record<string, PromptContext[]>>(
    {},
  );
  const contextLoads = new Map<string, Promise<void>>();
  const promptContext = () => contexts()[draftKey()] ?? [];
  async function loadPromptContext(key = draftKey()) {
    if (contexts()[key] !== undefined) return;
    if (contextLoads.has(key)) return contextLoads.get(key);
    const pending = window.haarness
      .storage("prompt-context:v1:" + key)
      .then((value) => {
        setContexts((items) =>
          items[key] !== undefined
            ? items
            : { ...items, [key]: readContext(value) },
        );
      })
      .finally(() => contextLoads.delete(key));
    contextLoads.set(key, pending);
    await pending;
  }
  createEffect(() => {
    const key = draftKey();
    if (state.ready) void run(() => loadPromptContext(key));
  });
  async function setPromptContext(key: string, items: PromptContext[]) {
    setContexts((values) => ({ ...values, [key]: items }));
    await window.haarness.storage("prompt-context:v1:" + key, items);
  }
  async function addPromptContext(item: PromptContext, key = draftKey()) {
    await loadPromptContext(key);
    await setPromptContext(key, addContext(contexts()[key] ?? [], item));
  }
  async function removePromptContext(item: PromptContext) {
    const key = draftKey();
    await setPromptContext(
      key,
      (contexts()[key] ?? []).filter(
        (value) => contextKey(value) !== contextKey(item),
      ),
    );
  }
  async function clearPromptContext(key: string, expected: PromptContext[]) {
    const items = contexts()[key];
    if (items?.some((item) => expected.includes(item)))
      await setPromptContext(
        key,
        items.filter((item) => !expected.includes(item)),
      );
  }
  async function selectProject(id: string, newChat = false) {
    set("openingChat", 0);
    const group = projects().find((item) => item.id === id);
    if (!group) return false;
    const location = primaryLocation(group);
    if (newChat) return connect({ recent: location.id }, true);
    const remembered = await window.haarness.storage("project-location:" + id);
    return activateProject(
      group.locations.find((item) => item.id === remembered)?.id ?? location.id,
    );
  }
  async function refreshProjects() {
    applyRecent(await window.haarness.projects());
  }
  async function startChat(location?: string) {
    batch(() => {
      set("openingChat", 0);
      set("loadingChat", 0);
    });
    if (location) {
      const target = state.recent.find((item) => item.id === location);
      if (!target || target.checkout?.exists === false) return false;
      await expandProject(projectKey(target), true);
      await expandLocation(location, true);
      if (activation?.id === location) newChatAttempt = connectAttempt;
      return connect({ recent: location }, true);
    }
    if (!project() && !state.openingProject) {
      await openProject(true);
      return;
    }
    newChatAttempt = connectAttempt;
    selection++;
    batch(() => {
      set("selected", "");
      set("items", []);
      set("panel", "chat");
    });
    const id = project()?.id;
    if (id) void window.haarness.storage("session:" + id, "");
    const group = currentProject();
    if (
      group &&
      !state.worktreeViews[group.id] &&
      primaryLocation(group).id !== id &&
      !activation
    )
      await connect({ recent: primaryLocation(group).id }, true);
  }
  function connect(
    input: Parameters<typeof window.haarness.connect>[0],
    newChat = false,
    inlineError = false,
  ) {
    if (input.recent && activation?.id === input.recent)
      return activation.promise;
    const promise = loadWorkspace(input, newChat, inlineError).finally(() => {
      if (activation?.promise === promise) activation = undefined;
    });
    activation = { id: input.recent, promise };
    return promise;
  }
  async function loadWorkspace(
    input: Parameters<typeof window.haarness.connect>[0],
    newChat = false,
    inlineError = false,
  ) {
    if (!(await leaveSettings()) || !(await leaveWorkspace())) return false;
    const attempt = ++connectAttempt;
    const connected = state.connection;
    const previous = {
      selected: state.selected,
      items: [...state.items],
      panel: state.panel,
    };
    set("openingProject", input.recent ?? input.workspace ?? "picker");
    if (input.recent && input.recent !== state.connection?.id) {
      selection++;
      batch(() => {
        set("loadingChat", 0);
        set("selected", "");
        set("items", []);
        set("panel", "chat");
      });
    }
    try {
      const view =
        input.recent && Object.keys(input).length === 1
          ? await projectConnection(input.recent)
          : await window.haarness.connect(input);
      if (!view || attempt !== connectAttempt) return false;
      set("projectFailure", null);
      const current = ++generation;
      const valid = () => current === generation && attempt === connectAttempt;
      forceSnapshot = true;
      lastCursor = 0;
      lastInstance = "";
      selection++;
      batch(() => {
        set("ready", false);
        set("loadingChat", 0);
        set("connectionStatus", "Connecting…");
        set("threads", []);
        set("turns", []);
        set("selected", "");
        set("items", []);
        set("tabs", []);
        tabHistory = [];
        set("streams", {});
        set("tasks", {});
        set("connection", view);
        set("activeServer", serverKey(view));
        set("permissions", []);
        set("questions", []);
      });
      const boot = await window.haarness.boot();
      if (!valid()) return false;
      set("servers", boot.servers);
      applyRecent(boot.recent);
      await window.haarness.storage("active-project", view.id);
      if (!valid()) return false;
      await window.haarness.storage("active-server", serverKey(view));
      if (!valid()) return false;
      await window.haarness.storage(
        "server-project:" + serverKey(view),
        view.id,
      );
      await window.haarness.storage(
        "project-location:" + projectKey(view),
        view.id,
      );
      if (state.expandedProjects[projectKey(view)] === undefined)
        await expandProject(projectKey(view), true);
      if (!valid()) return false;
      // establish() has just verified this connection. Reuse that snapshot
      // and load chat state and model choices together on remote connections.
      await Promise.all([hydrate(view.info), catalogs(false, view.info)]);
      if (!valid()) return false;
      if (newChat || newChatAttempt === attempt)
        await window.haarness.storage("session:" + view.id, "");
      const saved = await window.haarness.storage("session:" + view.id);
      if (!valid()) return false;
      if (
        !newChat &&
        newChatAttempt !== attempt &&
        typeof saved === "string" &&
        state.threads.some((item) => item.id === saved && !item.archived)
      )
        await choose(saved);
      else if (!newChat && newChatAttempt !== attempt && saved == null) {
        const waiting =
          state.permissions[0]?.rootThreadId ??
          state.permissions[0]?.threadId ??
          state.questions[0]?.rootThreadId ??
          state.questions[0]?.threadId;
        if (waiting) await choose(waiting);
      }
      if (!valid()) return false;
      set("ready", true);
      return true;
    } catch (error) {
      if (attempt !== connectAttempt) return false;
      if (!connected || state.connection?.id !== connected.id || !state.ready) {
        const request = connectAttempt + 1;
        await clearWorkspace(state.activeServer);
        if (request !== connectAttempt) return false;
        if (inlineError) throw error;
        set("projectFailure", {
          input,
          message: error instanceof Error ? error.message : String(error),
        });
        return false;
      }
      if (state.connection?.id !== input.recent) set(previous);
      throw error;
    } finally {
      if (attempt === connectAttempt) set("openingProject", "");
    }
  }
  async function activateProject(id: string) {
    if (state.recent.find((item) => item.id === id)?.checkout?.exists === false)
      throw new Error(
        "This checkout is missing. Restore its folder or choose another location in this project.",
      );
    if (
      state.connection?.id === id &&
      state.ready &&
      state.connectionStatus === "Connected" &&
      !activation
    )
      return true;
    return connect({ recent: id });
  }
  async function chooseProjectSession(project: string, thread: string) {
    const request = ++navigation;
    set("openingChat", request);
    try {
      if (!(await activateProject(project)) || state.openingChat !== request)
        return;
      await expandLocation(project, true);
      if (state.openingChat === request) await choose(thread);
    } finally {
      if (state.openingChat === request) set("openingChat", 0);
    }
  }
  let notificationNavigation = 0;
  async function openNotification(target: NotificationTarget) {
    try {
      await navigateNotification(target);
    } finally {
      await window.haarness.notificationHandled(target);
    }
  }
  async function navigateNotification(target: NotificationTarget) {
    const request = ++notificationNavigation;
    // Resolve against the captured location, never the currently selected server.
    // Validate before switching so a stale notification cannot open an empty chat.
    if (!state.recent.some((item) => item.id === target.connection))
      throw new Error(
        "This notification's project is no longer saved. Open the project to find the conversation.",
      );
    const view =
      state.connection?.id === target.connection &&
      state.connectionStatus === "Connected"
        ? state.connection
        : await projectConnection(target.connection);
    if (!view || request !== notificationNavigation) return;
    await window.haarness.api<T.Thread>(view.id, "getThread", [target.thread]);
    if (request !== notificationNavigation) return;
    if (!(await leaveSettings()) || request !== notificationNavigation) return;
    batch(() => {
      set("settings", null);
      set("settingsTarget", null);
      set("searchOpen", false);
      set("memoryOpen", false);
      set("remoteProjectRequest", null);
    });
    await expandProject(projectKey(view), true);
    if (request !== notificationNavigation) return;
    await chooseProjectSession(view.id, target.thread);
  }
  async function create(
    options: { provider?: string; model?: string; agent?: string } = {},
    onCreated?: (id: string) => void,
    panel: State["panel"] = "chat",
  ) {
    const connection = state.connection?.id;
    if (!connection) throw new Error("Open project first");
    const current = generation;
    const currentSelection = selection;
    const inheritBrowser = !state.selected;
    const thread = await window.haarness.api<T.Thread>(
      connection,
      "createThread",
      [options],
    );
    // The sender may clear the starter draft once it has a conversation owner.
    const initialDraft =
      state.drafts["new:" + (currentProject()?.id ?? connection)] ?? "";
    onCreated?.(thread.id);
    if (generation === current && state.connection?.id === connection) {
      set("threads", (items) => [
        thread,
        ...items.filter((item) => item.id !== thread.id),
      ]);
      if (!state.selected)
        set("drafts", sessionKey(connection, thread.id), initialDraft);
      if (selection === currentSelection) {
        if (inheritBrowser) {
          try { await window.haarness.browserAdopt(connection, thread.id); }
          catch (error) { set("error", "Your browser remains on the new-chat screen: " + String(error)); }
        }
        if (generation === current && selection === currentSelection) await choose(thread.id, panel);
      }
    }
    return thread;
  }
  async function send(
    text: string,
    attachments: string[] = [],
    effort?: string,
    options: { provider?: string; model?: string; agent?: string } = {},
    onCreated?: (id: string) => void,
    context: PromptContext[] = [],
    delivery?: T.InputDelivery,
  ) {
    const target = project()?.id;
    const draftProject = newDraftScope();
    const targetThread = state.selected;
    if (activation) await activation.promise;
    const connection = state.connection?.id;
    if (!connection) throw new Error("Open project first");
    if (connection !== target)
      throw new Error(
        "The project changed before sending. Your draft is saved.",
      );
    const targetApi = <V = unknown,>(method: string, ...args: unknown[]) =>
      window.haarness.api<V>(connection, method, args);
    // Resolve against this captured project immediately before admission.
    // Commands always execute now; explicit delivery still wins for messages.
    const command = text.match(/^\/([^\s]+)\s*([\s\S]*)$/);
    if (!delivery) {
      delivery =
        command ||
        !state.connection?.info.capabilities.includes("input-settings.v1")
          ? "steer"
          : (
              await targetApi<{ data: { delivery: T.InputDelivery } }>(
                "manageResource",
                "input-settings",
                { action: "get" },
              )
            ).data.delivery;
    }
    if (connection !== state.connection?.id || targetThread !== state.selected)
      throw new Error(
        "The project or conversation changed before sending. Your draft is saved.",
      );
    const commands = [...state.commands];
    const turns = [...state.turns];
    const thread = targetThread
      ? state.threads.find((t) => t.id === targetThread)
      : await create(options, onCreated);
    if (!thread) throw new Error("Session is unavailable");
    if (thread.isSubagent)
      throw new Error("Continue this task from its parent conversation.");
    // Keep existing conversations aligned with the connected model displayed
    // by the composer, including a fallback after credentials are removed.
    if (
      delivery === "steer" &&
      options.provider &&
      options.model &&
      (thread.provider !== options.provider || thread.model !== options.model)
    ) {
      await targetApi("updateThread", thread.id, {
        provider: options.provider,
        model: options.model,
      });
    }
    const key = "pending-input:" + connection + ":" + thread.id;
    const bytes = new TextEncoder().encode(
      JSON.stringify({
        text: promptWithContext(text, context),
        attachments,
        effort,
        delivery,
      }),
    );
    const fingerprint = Array.from(
      new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)),
    )
      .map((value) => value.toString(16).padStart(2, "0"))
      .join("");
    const previous = (await window.haarness.storage(key)) as {
      fingerprint: string;
      requestId: string;
      method: string;
    } | null;
    if (
      delivery === "queue" &&
      !state.connection?.info.capabilities.includes("input-queue.v1")
    )
      throw new Error(
        "This server does not support deferred messages. Update it before using Queue.",
      );
    if (delivery === "queue" && command)
      throw new Error(
        "Slash commands cannot be queued. Send a normal message or use Steer.",
      );
    const method =
      delivery === "queue"
        ? "queueInput"
        : command && commands.some((item) => item.name === command[1])
          ? "executeCommand"
          : turns.some((turn) => turn.threadId === thread.id)
            ? "queueInput"
            : "startTurn";
    const pending =
      previous?.fingerprint === fingerprint
        ? previous
        : { fingerprint, requestId: crypto.randomUUID(), method };
    await window.haarness.storage(key, pending);
    const input = {
      delivery,
      preview: true,
      tools: { preview: true },
      text: promptWithContext(text, context),
      attachments,
      requestId: pending.requestId,
      ...(effort ? { reasoningEffort: effort } : {}),
    };
    if (pending.method === "executeCommand" && command)
      await targetApi("executeCommand", thread.id, {
        name: command[1],
        tools: { preview: true },
        arguments: promptWithContext(command[2], context),
        requestId: pending.requestId,
      });
    else await targetApi(pending.method, thread.id, input);
    await window.haarness.storage(key, null);
    if (state.drafts[sessionKey(connection, thread.id)] === text)
      set("drafts", sessionKey(connection, thread.id), "");
    if (!targetThread && state.drafts["new:" + draftProject] === text)
      set("drafts", "new:" + draftProject, "");
    if (state.connection?.id === connection) await refresh();
    return thread.id;
  }
  async function settings(input: Partial<Preferences>) {
    set("preferences", await window.haarness.preferences(input));
  }
  function ordered(
    scope: string,
    ids: string[],
    newcomers: "start" | "end" = "end",
  ) {
    return orderedIds(ids, state.ordering[scope], newcomers);
  }
  async function reorder(
    scope: string,
    ids: string[],
    id: string,
    target: number,
  ) {
    if (!scope.startsWith("list:")) throw new Error("Invalid ordering scope");
    set(
      "ordering",
      scope,
      moveInOrder(state.ordering[scope] ?? ids, ids, id, target),
    );
    await window.haarness.storage("ordering:v1", {
      version: 1,
      lists: unwrap(state.ordering),
    });
  }
  async function boot() {
    const value = await window.haarness.boot();
    set(
      "modelRecents",
      readModelRecents(await window.haarness.storage("model-recents:v1")),
    );
    set(
      "approvalModes",
      readApprovalModes(await window.haarness.storage("approval-modes:v1")),
    );
    set("ordering", readOrdering(await window.haarness.storage("ordering:v1")));
    set(
      "projectMeta",
      readProjectMeta(
        await window.haarness.storage("project-presentation:v1"),
      ),
    );
    const expansion = (await window.haarness.storage(
      "project-expansion:v1",
    )) as { version?: unknown; projects?: Record<string, unknown> } | null;
    if (
      expansion?.version === 1 &&
      expansion.projects &&
      typeof expansion.projects === "object"
    ) {
      set(
        "expandedProjects",
        Object.fromEntries(
          Object.entries(expansion.projects).filter(
            (entry): entry is [string, boolean] =>
              typeof entry[1] === "boolean",
          ),
        ),
      );
    }
    for (const [key, field] of [
      ["worktree-views:v1", "worktreeViews"],
      ["worktree-expansion:v1", "expandedLocations"],
    ] as const) {
      const saved = await window.haarness.storage(key);
      if (saved && typeof saved === "object" && !Array.isArray(saved))
        set(
          field,
          Object.fromEntries(
            Object.entries(saved).filter(
              ([, value]) => typeof value === "boolean",
            ),
          ),
        );
    }
    set("preferences", value.preferences);
    set("servers", value.servers);
    applyRecent(value.recent);
    set("version", value.version);
    set("platform", value.platform);
    const savedProject = await window.haarness.storage("active-project");
    const savedServer = await window.haarness.storage("active-server");
    const legacy = value.recent.find((item) => item.id === savedProject);
    const server =
      typeof savedServer === "string" &&
      servers().some((item) => item.id === savedServer)
        ? savedServer
        : savedServer == null && legacy
          ? serverKey(legacy)
          : "local";
    set("activeServer", server);
    const remembered = await window.haarness.storage(
      "server-project:" + server,
    );
    const locations = serverProjects(value.recent, server);
    const project = locations.find(
      (item) => item.id === (remembered ?? savedProject),
    );
    if (project) await connect({ recent: project.id });
    else set("ready", true);
    const target = await window.haarness.notificationReady();
    if (target) await openNotification(target);
  }
  const off = window.haarness.onEvent((event: DesktopEvent) => {
    if (event.kind === "notification-open") {
      void run(() => openNotification(event.data as NotificationTarget));
      return;
    }
    if (event.kind === "projects") {
      applyRecent(event.data as Recent[]);
      return;
    }
    if (event.kind === "menu") {
      menuHandler(event.data);
      return;
    }
    if (
      event.kind === "connection" &&
      (!event.connection || event.connection === state.connection?.id)
    ) {
      if (event.id && event.id !== subscription) return;
      reconnect();
      return;
    }
    if (event.kind !== "server" || event.connection !== state.connection?.id)
      return;
    const item = event.data as T.EventEnvelope | { type: "resync_required" };
    if (item.type === "resync_required") {
      forceSnapshot = true;
      reconnect();
      return;
    }
    const envelope = item as T.EventEnvelope;
    if (envelope.serverInstanceId !== lastInstance) {
      forceSnapshot = true;
      reconnect();
      return;
    }
    if (envelope.eventId <= lastCursor) return;
    lastCursor = envelope.eventId;
    for (const journal of journals) journal.push(item as T.EventEnvelope);
    reduceEvent(state, set, item as T.EventEnvelope);
    if (envelope.type === "item.created" && envelope.threadId === state.selected && !state.loadingChat) {
      const entry = envelope.payload as T.Item;
      const data = entry.data;
      if (data?.type === "tool_result" && data.name === "preview" && !data.is_error) {
        const info = previewInfo(data.metadata);
        if (info) set("fileRequest", { connection: state.connection!.id, path: info.path, mode: info.mode, thread: state.selected, preview: true, sequence: envelope.eventId });
      }
    }

    if (item.type === "permission.resolved")
      retainPermissionErrors(state.connection!.id, state.permissions);
    if (item.type === "assets.updated")
      void catalogs().catch((error) => set("error", String(error)));
    if (
      ["thread.reverted", "thread.unreverted", "thread.compacted"].includes(
        item.type,
      ) &&
      "threadId" in item &&
      item.threadId === state.selected
    ) {
      const connection = state.connection?.id;
      const selected = state.selected;
      void api<T.Item[]>("listItems", selected)
        .then((items) => {
          if (
            state.connection?.id === connection &&
            state.selected === selected
          )
            set("items", items);
        })
        .catch((error) => {
          if (
            state.connection?.id === connection &&
            state.selected === selected
          )
            set("error", String(error));
        });
    }
  });
  const systemTheme = matchMedia("(prefers-color-scheme: dark)");
  const updateAppearance = () => {
    const appearance =
      state.preferences.theme === "system"
        ? systemTheme.matches
          ? "dark"
          : "light"
        : state.preferences.theme;
    const root = document.documentElement;
    root.dataset.theme = appearance;
    root.dataset.palette = state.preferences.palette;
    root.style.colorScheme = appearance;
    for (const [key, color] of Object.entries(
      themeColors(state.preferences.palette, appearance),
    ))
      root.style.setProperty("--" + key, color);
    root.style.setProperty("--font-size", state.preferences.fontSize + "px");
  };
  systemTheme.addEventListener("change", updateAppearance);
  onCleanup(() => systemTheme.removeEventListener("change", updateAppearance));
  createEffect(updateAppearance);
  onCleanup(() => {
    stopped = true;
    generation++;
    off();
    clearTimeout(reconnectTimer);
    clearTimeout(noticeTimer);
    if (subscription) void window.haarness.unsubscribe(subscription);
  });
  return {
    state,
    isAutoApproval,
    setAutoApproval,
    allowOnceAndAuto,
    set,
    api,
    native,
    run,
    notify,
    connect,
    projectConnection,
    projects,
    servers,
    currentServer,
    switchServer,
    openProject,
    forgetProject,
    removeServer,
    applyRecent,
    indexes,
    conversations,
    currentProject,
    draftKey,
    attachmentDraft,
    loadAttachmentDraft,
    setAttachmentDraft,
    addAttachments,
    attachFiles,
    promptContext,
    loadPromptContext,
    addPromptContext,
    removePromptContext,
    setPromptContext,
    clearPromptContext,
    newDraftScope,
    setWorktreeView,
    updateProjectMeta,
    renameSection,
    removeSection,
    fileProjectBeside,
    fileProjectInSection,
    archiveProjectChats,
    revealProject,
    expandLocation,
    selectProject,
    refreshProjects,
    expandProject,
    activateProject,
    project,
    startChat,
    chooseProjectSession,
    choose,
    closeTab,
    create,
    send,
    refresh,
    permissionError,
    recordPermissionError,
    hydrate,
    rememberModel,
    catalogs,
    settings,
    boot,
    ordered,
    reorder,
    registerSearch,
    searchCommands: () =>
      Object.values(registrations()).flatMap((entries) => entries()),
    /** Open the connect-a-provider dialog; `onboarding` when nothing works yet. */
    openConnect: (options: { provider?: string; onboarding?: boolean } = {}) => {
      if (!state.connection) return;
      set("connect", {
        ...options,
        onboarding:
          options.onboarding ??
          !state.providers.some((provider) => provider.authenticated && provider.models.length),
      });
    },
    openSettings: async (section: string, target: string | null = null) => {
      if (state.settings && !(await leaveSettings())) return false;
      batch(() => {
        set("settingsTarget", target);
        set("settings", section);
        set("settingsRevision", (revision) => revision + 1);
      });
      return true;
    },
    closeSettings: async () => {
      if (!(await leaveSettings())) return false;
      batch(() => {
        set("settings", null);
        set("settingsTarget", null);
      });
      return true;
    },
    canLeaveSettings: () => leaveSettings(),
    canLeaveWorkspace: () => leaveWorkspace(),
    onLeaveSettings: (fn: typeof leaveSettings) => (leaveSettings = fn),
    onLeaveWorkspace: (fn: typeof leaveWorkspace) => (leaveWorkspace = fn),
    onMenu: (fn: typeof menuHandler) => (menuHandler = fn),
  };
}
type AppContext = ReturnType<typeof createApp>;
const Context = createContext<AppContext>();
export const AppProvider = (props: ParentProps) => (
  <Context.Provider value={createApp()}>{props.children}</Context.Provider>
);
export function useApp() {
  return useContext(Context)!;
}
