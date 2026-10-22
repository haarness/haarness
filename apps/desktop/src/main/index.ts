import { accountRequest, accountLogin, cancelAccountLogin, connectCloud, closeCloudConnections, billingState, startBillingWatch, stopBillingWatch } from "./account";
import { createReviewStorage, type ReviewRequest } from "../review-drafts";
import { browserLinkDestination } from "../browser-settings";
import { browserData, removeBrowserData, saveBrowserDownload } from "./browser-data";
import { createBrowserHost } from "./browser-host";
import { chromeSetup, revealChromeExtension, setupChromeOnLaunch } from "./chrome-setup";
import { createAttachmentDraftStorage } from "./attachment-drafts";
import { defaultScreenshotFolder, screenshotThumbnails } from "./screenshot-folder";
import { listScreenshots, readListedScreenshots, type FileIdentity } from "./screenshot-files";
import { createScreenshotIntents, createScreenshotListings, displayFolder, inputKind } from "../screenshots";
import { terminalSockets } from "./terminal-sockets";
import { translate } from "../localization";
import { globalAccelerator, globalShortcutError, type GlobalShortcut } from "../global-shortcut";
import { shortcutLabel } from "../renderer/search";
/** Native dialogs speak the language chosen in Settings, like the menus do. */
const say = (key: string) => translate(data().preferences.language, key);
import { authorizationHeaders, type ServerAuth } from "@haarness/sdk";
import { browseRemote, openRemote, remoteLocationId } from "./remote-projects";
import { withThreadKinds } from "../thread-kinds";
import {
  app,
  BrowserWindow,
  ipcMain,
  dialog,
  shell,
  Menu,
  Notification,
  powerMonitor,
  nativeTheme,
  nativeImage,
  globalShortcut,
} from "electron";
import { readFile, writeFile, realpath, stat } from "node:fs/promises";
import { homedir } from "node:os";
import { join, resolve, basename, sep, isAbsolute } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { randomUUID } from "node:crypto";
import {
  Haarness,
  attachments,
  type PtyConnection,
  type JsonValue,
  type WorkspaceListResponse,
  type WorkspaceOperationResponse,
} from "@haarness/sdk";
import { z } from "zod";
import type { Connection, DesktopEvent, Recent, NotificationTarget } from "../shared";
import { themeColors } from "../themes";
import { serverKey } from "../servers";
import {
  mergeProjectCatalog,
  projectKey,
  rememberConnected,
} from "../projects";
import {
  connectLocal,
  establish,
  connection,
  connections,
  invoke,
  command,
  commands,
  stopCommand,
} from "./runtime";
import {
  load,
  data,
  persist,
  written,
  preferences,
  serverAuth,
  saveServerAuth,
} from "./store";
import { bounded, external, request } from "./validation";
import { fetchRemoteImage } from "./remote-image";
import { createVoiceInput, launchVoice } from "./voice";
import { createProjectQueue } from "./project-queue";
import { createUiSession } from "./ui-session";
import { createNotifications } from "./notifications";

if (process.env.HAARNESS_DESKTOP_DATA)
  app.setPath("userData", resolve(process.env.HAARNESS_DESKTOP_DATA));
app.setName("Haarness");
// Playwright's loader for an unpackaged app appends --password-store=basic,
// over any store on the command line, and the store refuses plain text for
// remote-server credentials. A Linux test session that brings its own
// unlocked keyring (scripts/linux-desktop-session.sh) names that store here.
if (
  process.platform === "linux" &&
  process.env.HAARNESS_DESKTOP_TEST === "1" &&
  process.env.HAARNESS_DESKTOP_PASSWORD_STORE
)
  app.commandLine.appendSwitch("password-store", process.env.HAARNESS_DESKTOP_PASSWORD_STORE);
let initialized = false;
let quitting = false;
// Pending startup work can resume while Electron destroys web contents. Creating
// a renderer after its main loop exits is a native fatal error, not a JS error.
app.on("before-quit", () => {
  quitting = true;
});
app.on("will-quit", () => {
  quitting = true;
});
// The last saves (open browser tabs, the window size) start as the app quits;
// let them reach the disk before the process exits, for two seconds at most.
let saved = false;
app.on("will-quit", (event) => {
  if (saved) return;
  saved = true;
  event.preventDefault();
  void Promise.race([
    written(),
    new Promise((resolve) => setTimeout(resolve, 2000)),
  ]).then(() => app.exit());
});
app.on("quit", () => {
  quitting = true;
});
// A development run boots the stock Electron bundle, which brings its own Dock
// icon. Point it at ours so the app looks like itself before it is packaged;
// a packaged build gets the icon from the bundle instead.
if (!app.isPackaged && process.platform === "darwin")
  app.whenReady().then(() => {
    if (quitting) return;
    const brand = nativeImage.createFromPath(
      resolve(fileURLToPath(new URL(".", import.meta.url)), "../../assets/icon.png"),
    );
    if (!brand.isEmpty()) app.dock?.setIcon(brand);
  });
const here = fileURLToPath(new URL(".", import.meta.url));
let renderer = resolve(here, "../renderer/index.html");
let preload = resolve(here, "../preload/index.cjs");
let uiSession: Awaited<ReturnType<typeof createUiSession>> | undefined;
const windows = new Set<BrowserWindow>();
const subscriptions = new Map<string, { owner: number; client: Haarness }>();
// Test runs never share usage statistics. Every server and CLI process
// inherits this.
if (process.env.HAARNESS_DESKTOP_TEST === "1" && process.env.HAARNESS_TELEMETRY === undefined)
  process.env.HAARNESS_TELEMETRY = "0";
const commandOwners = new Map<string, number>();
const terminals = terminalSockets<PtyConnection>();
// End-to-end tests end terminal sockets as a server or network outage does,
// and keep the outage going by refusing new ones while
// `haarnessTerminalOutage` is set.
if (process.env.HAARNESS_DESKTOP_TEST === "1")
  Object.assign(globalThis, { haarnessTerminalSockets: terminals, haarnessTerminalOutage: false });
const terminalOutage = () =>
  process.env.HAARNESS_DESKTOP_TEST === "1" &&
  (globalThis as { haarnessTerminalOutage?: boolean }).haarnessTerminalOutage === true;
/** Whether a terminal socket still takes input. The SDK keeps its WebSocket
 * to itself; while that cannot be read, the socket counts as open. */
function socketOpen(socket: PtyConnection) {
  const state = (socket as unknown as { socket?: { readyState?: number } }).socket?.readyState;
  return state === undefined || state === 1;
}
const projectTransaction = createProjectQueue();
/** Each window's last screenshot listing; the renderer picks by place in it. */
const screenshotListings = createScreenshotListings<{ path: string; identity: FileIdentity }>();
/** Each window's presses and Attach menu: the screenshots folder is listed for that menu's choice. */
const screenshotIntents = createScreenshotIntents();
const worktreeOperations = new Set<string>();
const voice = createVoiceInput((locale) =>
  launchVoice(
    process.env.HAARNESS_DESKTOP_TEST === "1" &&
      process.env.HAARNESS_VOICE_TEST_HELPER
      ? process.env.HAARNESS_VOICE_TEST_HELPER
      : app.isPackaged
        ? join(process.resourcesPath, "bin/haarness-voice")
        : resolve(here, "../../resources/haarness-voice"),
    locale,
  ),
);
// Windows and Linux start the app with a clicked haarness:// link as an
// argument (macOS sends open-url instead); hold it until a window can open it.
let pendingLink: string | undefined =
  process.platform === "darwin"
    ? undefined
    : process.argv.find((arg) => arg.startsWith("haarness://"));
let updateReady = false,
  updateAvailable = false,
  updateListeners = false;
const send = (win: BrowserWindow, event: DesktopEvent) => {
  if (!win.isDestroyed()) win.webContents.send("haarness:event", event);
};
const browserHost = createBrowserHost(send);
const broadcast = (event: DesktopEvent) =>
  windows.forEach((win) => send(win, event));
const notificationReady = new Set<number>();
const notificationPending = new Map<number, NotificationTarget>();
const notifications = createNotifications({
  enabled: () => data().preferences.notifications && Notification.isSupported(),
  policy: () => data().preferences,
  focused: () =>
    [...windows].some((win) => !win.isDestroyed() && win.isFocused()),
  create: (options) => new Notification(options),
  translate: say,
  open: (target, owner) => {
    const win =
      [...windows].find((win) => win.id === owner) ??
      [...windows][0] ??
      createWindow();
    if (!win || quitting) return;
    if (win.isMinimized()) win.restore();
    win.show();
    // An explicit notification click should activate a background macOS app,
    // not only focus its window within that app.
    if (process.platform === "darwin") app.focus({ steal: true });
    win.focus();
    notificationPending.set(win.id, target);
    if (notificationReady.has(win.id))
      send(win, { kind: "notification-open", data: target });
  },
});
// Independent of renderer/sidebar subscriptions, so collapsing a project or
// reloading/closing the macOS window does not lose a running task's alert.
const notificationWatches = new Map<
  string,
  {
    client: Haarness;
    endpoint: string;
    auth: string;
    owner: number;
    stop: () => void;
  }
>();
function watchNotifications(id: string, owner: number) {
  const item = connection(id);
  const previous = notificationWatches.get(id);
  const auth = JSON.stringify(item.auth);
  if (previous?.endpoint === item.url && previous.auth === auth) {
    previous.owner = owner;
    return;
  }
  previous?.stop();
  const client = new Haarness({ baseUrl: item.url, auth: item.auth });
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const watch = {
    client,
    endpoint: item.url,
    auth,
    owner,
    stop() {
      stopped = true;
      clearTimeout(timer);
      client.close();
    },
  };
  notificationWatches.set(id, watch);
  let cursor = item.view.info.eventCursor ?? 0;
  let instance = item.view.info.serverInstanceId;
  let retry = 0;
  async function listen() {
    try {
      const info = await client.info();
      if (info.serverInstanceId !== instance) {
        instance = info.serverInstanceId;
        cursor = info.eventCursor ?? 0;
      }
      for await (const event of client.events({
        after: cursor,
        reconnect: true,
      })) {
        if (stopped) return;
        if (!("eventId" in event)) {
          const info = await client.info();
          instance = info.serverInstanceId;
          cursor = info.eventCursor ?? 0;
          break;
        }
        cursor = event.eventId;
        retry = 0;
        void notifications
          .receive(
            id,
            projectLabel(connections.get(id)?.view ?? item.view),
            watch.owner,
            event,
            (thread) => client.getThread(thread),
          )
          .catch(() => {});
      }
    } catch {
      /* Retry without disturbing the user's foreground connection. */
    }
    if (!stopped)
      timer = setTimeout(
        () => void listen(),
        Math.min(30000, 1000 * 2 ** Math.min(retry++, 5)),
      );
  }
  void listen();
}
/** The name the sidebar shows for a project: its name from Edit project,
 * else the catalog's. Read when a notification goes out, so a rename since
 * the project was opened is already in it. */
function projectLabel(view: Connection) {
  const presentation = data().values["project-presentation:v1"] as
    | { projects?: Record<string, { name?: unknown } | undefined> }
    | undefined;
  const name = presentation?.projects?.[projectKey(view)]?.name;
  return typeof name === "string" && name.trim()
    ? name.trim()
    : (view.project?.name ?? view.label);
}
function forgetNotifications(id: string) {
  notificationWatches.get(id)?.stop();
  notificationWatches.delete(id);
  notifications.forget(id);
}
let projectCatalog: Promise<Recent[]> | undefined;
async function refreshProjects(): Promise<Recent[]> {
  if (projectCatalog) return projectCatalog;
  projectCatalog = (async () => {
    const local = data().recent.filter((item) => !item.remote);
    let directory: Recent | undefined;
    for (const item of local) {
      if (
        await stat(item.workspace)
          .then((value) => value.isDirectory())
          .catch(() => false)
      ) {
        directory = item;
        break;
      }
    }
    if (!directory) return data().recent;
    const result = await invoke(directory.workspace, false, ["desktop"], {
      action: "projects",
      directories: local.map((item) => item.workspace),
    });
    const recent = mergeProjectCatalog(
      data().recent,
      result.value as WorkspaceListResponse,
    );
    if (JSON.stringify(recent) !== JSON.stringify(data().recent)) {
      data().recent = recent;
      for (const item of recent) {
        const current = connections.get(item.id);
        if (current)
          Object.assign(current.view, {
            project: item.project,
            checkout: item.checkout,
          });
      }
      await persist();
      broadcast({ kind: "projects", data: recent });
    }
    return data().recent;
  })().finally(() => {
    projectCatalog = undefined;
  });
  return projectCatalog;
}
function source(url: string) {
  return (
    url === pathToFileURL(renderer).href ||
    (!!process.env.ELECTRON_RENDERER_URL &&
      new URL(url).origin === new URL(process.env.ELECTRON_RENDERER_URL).origin)
  );
}
function handle(
  name: string,
  fn: (win: BrowserWindow, ...args: never[]) => unknown,
) {
  ipcMain.handle("haarness:" + name, async (event, ...args) => {
    try {
      const win = BrowserWindow.fromWebContents(event.sender);
      if (
        !win ||
        !windows.has(win) ||
        event.senderFrame !== event.sender.mainFrame ||
        !source(event.senderFrame.url)
      )
        throw new Error("Untrusted desktop request");
      bounded(args);
      return { ok: true, value: await fn(win, ...(args as never[])) };
    } catch (error) {
      let message = error instanceof Error ? error.message : String(error);
      for (const { auth } of connections.values()) {
        const secret =
          auth.type === "bearer"
            ? auth.token
            : auth.type === "basic"
              ? auth.password
              : "";
        if (secret) message = message.replaceAll(secret, "[redacted]");
      }
      return { ok: false, error: message.slice(0, 8000) };
    }
  });
}
// Checkout management must quiesce the idle daemon that the sidebar opens.
// Hold the same per-location queue as connects, so a reconnect cannot acquire
// the workspace lease while reset/removal is in progress.
async function manageWorktree(owner: string, method: string, args: unknown[]) {
  if (
    !args[1] ||
    typeof args[1] !== "object" ||
    (args[1] as { confirm?: unknown }).confirm !== true
  )
    throw new Error("Confirm the worktree operation before proceeding.");
  const root = connection(owner);
  if (root.view.remote)
    throw new Error("Manage worktrees from a local project.");
  const workspace = await root.client.getWorkspace(String(args[0]));
  if (!workspace.managed || workspace.kind !== "git_worktree")
    throw new Error("Only managed worktrees can be reset or removed.");
  const id = "local:" + workspace.path;
  if (owner === id)
    throw new Error("Manage this worktree from the project's root folder.");
  return projectTransaction(id, async () => {
    worktreeOperations.add(id);
    try {
      if (workspace.exists) {
        const saved = data().recent.find((item) => item.id === id);
        await connectLocal(workspace.path, saved?.trust ?? root.view.trust);
      }
      const cached = connections.get(id);
      const target =
        !workspace.exists &&
        cached &&
        !(await cached.client.health({ timeoutMs: 1000 }).then(
          () => true,
          () => false,
        ))
          ? undefined
          : cached;
      if (target) {
        // Fail closed if activity cannot be read. Reset/removal never closes
        // an interactive terminal or answers a pending approval on the user's behalf.
        const [turns, ptys, workflows, permissions, questions] =
          await Promise.all([
            target.client.activeTurns(),
            target.client.listPtys(),
            target.client.listWorkflowRuns(),
            target.client.listPermissions(),
            target.client.listQuestions(),
          ]);
        if (
          turns.length ||
          permissions.length ||
          questions.length ||
          ptys.some((item) => item.status === "running") ||
          workflows.some(
            (item) => item.status === "running" || item.status === "paused",
          )
        )
          throw new Error(
            "Finish this worktree's active tasks and close its terminals before resetting or removing it.",
          );
        await target.client.shutdownServer();
        connections.delete(id);
        target.client.close();
      }
      const operation = (
        root.client as unknown as Record<
          string,
          (...args: unknown[]) => Promise<WorkspaceOperationResponse>
        >
      )[method];
      const deadline = Date.now() + 5000;
      let result: WorkspaceOperationResponse;
      for (;;) {
        try {
          result = await operation.apply(root.client, args);
          break;
        } catch (error) {
          // The listener closes just before the process drops its lease.
          if (
            !target ||
            Date.now() >= deadline ||
            !String(error).includes("workspace has an active server")
          )
            throw error;
          await new Promise((resolve) => setTimeout(resolve, 50));
        }
      }
      if (method !== "resetWorkspace") {
        connections.get(id)?.client.close();
        connections.delete(id);
        data().recent = data().recent.filter((item) => item.id !== id);
        await persist();
      }
      return result;
    } finally {
      worktreeOperations.delete(id);
    }
  });
}

const apis = new Set(
  `manageResource info analyticsStats analyticsInsights providers refreshProviders agents commands lspStatus formatterStatus toolCatalog updateLazyTools updateTool projects workspacesList createWorkspace workspaceRefs resolveWorkspaceRef getWorkspace removeWorkspace openWorkspace resetWorkspace cleanupWorkspace listPtys createPty getPty updatePty deletePty workflows workflowSource startWorkflow listWorkflowRuns getWorkflowRun deleteWorkflowRun waitWorkflowRun cancelWorkflowRun pauseWorkflowRun resumeWorkflowRun skills loadSkill plugins pluginAction mcpServers configureMcp reconnectMcp disconnectMcp mcpTools callMcpTool mcpResources mcpResourceTemplates readMcpResource mcpPrompts getMcpPrompt listThreads createThread getThread updateThread exportSessionArchive forkThread listItems listTodos activeTurn interruptThreadTree listBackgroundTasks promoteBackgroundTasks pendingQuestion startTurn executeCommand queueInput inputQueue changeInputQueue btw compact getGoal setGoal getApprovalMode setApprovalMode getLoop setLoop threadUsage revertThread unrevertThread lastTurnDiff lastTurnReview vcsDiff searchFiles importSessionArchive activeTurns getTurn interruptTurn waitTurn listPermissions replyPermission listQuestions replyQuestion shutdownServer health openapi listProfiles uploadProfile getProfile deleteProfile downloadProfile listBrowserHosts`.split(
    " ",
  ),
);
function installHandlers() {
  handle("voice", (win, action: unknown, id: unknown, locale: unknown) => {
    const input = z
      .object({
        action: z.enum(["start", "stop", "cancel"]),
        id: z.string().uuid(),
        locale: z.string().max(80).optional(),
      })
      .parse({ action, id, locale });
    if (input.action !== "start") {
      voice.stop(win.id, input.id, input.action === "cancel");
      return;
    }
    if (process.platform !== "darwin")
      throw new Error("Native dictation is currently available on macOS.");
    voice.start(win.id, input.id, input.locale ?? "en-US", (value) =>
      send(win, { kind: "voice", id: input.id, data: value }),
    );
  });
  handle("browserAttach", (win, connection:string, thread:string) => browserHost.attach(win, z.string().max(2048).parse(connection), z.string().max(128).parse(thread)));
  handle("browserAdopt", (win, connection:string, thread:string) => browserHost.adopt(win, z.string().max(2048).parse(connection), z.string().min(1).max(128).parse(thread)));
  handle("browserCommand", (win, connection:string, thread:string, action:unknown) => browserHost.command(win, z.string().max(2048).parse(connection), z.string().max(128).parse(thread), action));
  handle("browserLayout", (win, connection:string, thread:string, raw:unknown) => browserHost.layout(win, connection, thread, z.object({visible:z.boolean(),bounds:z.object({x:z.number().finite(),y:z.number().finite(),width:z.number().finite(),height:z.number().finite()})}).parse(raw)));
  handle("browserSnapshot", (win, connection:string, thread:string) => browserHost.snapshot(win, z.string().max(2048).parse(connection), z.string().max(128).parse(thread)));
  handle("projects", () => refreshProjects());
  handle(
    "remoteProjects",
    async (_, rawServer: unknown, rawPath: unknown, rawOptions: unknown) => {
      const id = z.string().parse(rawServer);
      const path = z.string().max(16384).optional().parse(rawPath);
      const server = data().servers.find((item) => item.id === id);
      if (!server?.url) throw new Error("Saved remote server was not found");
      const options = z
        .object({
          prefix: z.string().max(4096).optional(),
          hidden: z.boolean().optional(),
          limit: z.number().int().min(1).max(200).optional(),
          after: z.string().max(4096).optional(),
        })
        .strict()
        .optional()
        .parse(rawOptions);
      return {
        listing: await browseRemote(server.url, serverAuth(id), path, options),
      };
    },
  );
  handle("notificationHandled", (win, raw: unknown) => {
    const target = z
      .object({ connection: z.string(), thread: z.string() })
      .strict()
      .parse(raw);
    const pending = notificationPending.get(win.id);
    if (
      pending?.connection === target.connection &&
      pending.thread === target.thread
    )
      notificationPending.delete(win.id);
  });
  handle("notificationReady", (win) => {
    notificationReady.add(win.id);
    const target = notificationPending.get(win.id) ?? null;
    return target;
  });
  handle("boot", () => ({
    preferences: data().preferences,
    recent: data().recent,
    servers: data().servers,
    version: app.getVersion(),
    platform: process.platform,
    updateConfigured: !!process.env.HAARNESS_UPDATE_URL,
  }));
  handle("preferences", async (_, input) => {
    // A shortcut another app holds is refused before it is saved, and the
    // one saved before stays registered.
    const shortcut = (input as { globalShortcut?: GlobalShortcut } | undefined)?.globalShortcut;
    const problem = shortcut && globalShortcutError(shortcut.keys, process.platform, say);
    if (problem) throw new Error(problem);
    if (shortcut && !registerShortcut(shortcut)) {
      registerShortcut();
      throw new Error(
        say("Another app or the system already uses {keys}. Choose another shortcut.").replace(
          "{keys}",
          shortcutLabel(shortcut.keys, process.platform),
        ),
      );
    }
    const value = await preferences(input).catch((error) => {
      if (shortcut) registerShortcut();
      throw error;
    });
    menu();
    repaintWindows();
    broadcast({ kind: "menu", data: { action: "preferences", value } });
    return value;
  });
  // While Settings records a new global shortcut, the current one must not
  // hide the window.
  handle("globalShortcut", (win, action: unknown) => {
    if (action !== "suspend") return resumeShortcut(win.id);
    shortcutSuspendedBy = win.id;
    registerShortcut();
  });
  const attachmentDraftStorage = createAttachmentDraftStorage(join(app.getPath("userData"), "attachment-drafts"));
  handle("attachmentDraft", (_, key: string, value: unknown) => attachmentDraftStorage(key, value));
  const reviewStorage = createReviewStorage(data().values, persist);
  handle("review", async (_, input: ReviewRequest) => {
    bounded(input, 2 * 1024 * 1024);
    const target = z.object({ connection: z.string().min(1).max(200), thread: z.string().uuid() });
    const draft = z.object({ id: z.string().uuid(), text: z.string(), target: target.optional() });
    const scope = z.string().min(1).max(1000);
    const anchor = z.string().min(1).max(4000);
    const request = z.discriminatedUnion("action", [
      z.object({ action: z.literal("load"), scope }),
      z.object({ action: z.literal("save"), scope, anchor, draft }),
      z.object({ action: z.literal("pin"), scope, anchor, draft }),
      z.object({ action: z.literal("remove"), scope, anchor, id: z.string().uuid() }),
      z.object({ action: z.literal("deliver"), scope, anchor, draft, note: z.string(), current: z.string() }),
    ]).parse(input);
    if (request.action === "deliver") {
      const destination = request.draft.target;
      const source = JSON.parse(request.scope) as unknown;
      if (!destination || !Array.isArray(source) || source[0] !== destination.connection
        || (source[1] && source[1] !== destination.thread))
        throw new Error("The review belongs to another project or chat");
      await connection(destination.connection).client.getThread(destination.thread);
    }
    return reviewStorage(request);
  });
  handle("storage", async (_, key: string, value: unknown) => {
    if (
      typeof key !== "string" ||
      key.length > 400 ||
      ["__proto__", "constructor", "prototype"].includes(key)
    )
      throw new Error("Invalid storage key");
    if (value !== undefined) {
      bounded(value, 2 * 1024 * 1024);
      if (value === null) delete data().values[key];
      else data().values[key] = value;
      await persist();
    }
    return data().values[key] ?? null;
  });
  handle("account", async (win, raw: unknown) => {
    const input = z.object({ action: z.enum(["status", "login", "cancel", "logout", "select", "routers", "select-router", "workspaces", "create", "start", "stop", "agents", "sessions", "create-session", "session-start", "session-stop", "billing", "billing-state", "usage", "web"]), id: z.string().uuid().optional(), name: z.string().max(200).optional(), repo: z.string().max(2000).optional(), configuration: z.record(z.string(), z.unknown()).optional(), page: z.enum(["billing", "topup", "usage", "agents", "sessions", "session-settings"]).optional(), device: z.boolean().optional() }).strict().parse(raw);
    if (input.action === "billing-state") return billingState();
    if (input.action === "cancel") { cancelAccountLogin(); return null; }
    if (input.action === "login") {
      await accountLogin(!!input.device, value => send(win, { kind: "menu", data: { action: "account-login", ...value as object } }));
    } else if (input.action === "web") {
      const result = await accountRequest(input); await shell.openExternal(external(result.url)); return null;
    } else {
      if (["logout", "select"].includes(input.action)) cancelAccountLogin();
      if (input.action === "logout") closeCloudConnections();
      const result = await accountRequest(input);
      if (!["logout", "select", "select-router"].includes(input.action)) return result;
      if (input.action !== "select-router") closeCloudConnections();
    }
    await Promise.allSettled([...connections.values()].filter(c => !c.view.remote).map(c => c.client.refreshProviders()));
    startBillingWatch(state => broadcast({ kind: "menu", data: { action: "account-billing", state } }));
    broadcast({ kind: "menu", data: { action: "account-changed" } });
    return accountRequest({ action: "status" });
  });
  handle("connect", async (win, raw: unknown) => {
    const input = z
      .object({
        workspace: z.string().optional(),
        cloudWorkspace: z.string().uuid().optional(),
        cloudSession: z.string().uuid().optional(),
        url: z.string().optional(),
        token: z.string().optional(),
        username: z.string().max(1024).optional(),
        password: z.string().max(8192).optional(),
        label: z.string().optional(),
        trust: z.boolean().optional(),
        recent: z.string().optional(),
        /** The folder came from a haarness:// link rather than the user's own choice. */
        deepLink: z.boolean().optional(),
      })
      .strict()
      .parse(raw);
    // Resolving a network path would already reach its host.
    if (
      input.deepLink &&
      (!input.workspace ||
        !isAbsolute(input.workspace) ||
        /^[\\/]{2}/.test(input.workspace) ||
        input.url ||
        input.recent ||
        input.cloudSession ||
        input.cloudWorkspace)
    )
      throw new Error("A link can only open a folder on this computer");
    const recent = input.recent
      ? data().recent.find((item) => item.id === input.recent)
      : undefined;
    if (input.recent && !recent) throw new Error("Saved project was not found");
    const cloudSession = input.cloudSession ?? recent?.cloudSession;
    const cloudWorkspace = input.cloudWorkspace ?? recent?.cloudWorkspace;
    if (cloudSession || cloudWorkspace) {
      const view = await connectCloud((cloudSession ?? cloudWorkspace)!, recent, !!cloudSession);
      // Removed from the sidebar while it connected: it stays removed.
      const kept = await rememberConnected({
        recent: () => data().recent,
        saved: view,
        requested: input.recent,
        save: async (next) => {
          data().recent = next;
          await persist();
        },
        watch: () => watchNotifications(view.id, win.id),
      });
      return kept ? view : null;
    }

    let workspace = input.workspace ?? recent?.workspace;
    const url = input.url ?? recent?.url;
    if (!url && !workspace) {
      const result = await dialog.showOpenDialog(win, {
        title: say("Open project"),
        properties: ["openDirectory", "createDirectory"],
      });
      if (result.canceled) return null;
      workspace = result.filePaths[0];
    }
    if (input.deepLink)
      workspace = await realpath(workspace!).catch(() => {
        throw new Error(say("The folder in this link was not found."));
      });
    else if (!url) workspace = await realpath(workspace!);
    // A haarness:// link can name any folder. One already in the sidebar
    // opens as it was saved; any other is confirmed first and opens with its
    // project configuration off, so a link can never start a folder's MCP
    // servers, hooks or extensions without the user choosing to trust it.
    let linkTrust: boolean | undefined;
    if (input.deepLink) {
      const saved = data().recent.find(
        (item) => !item.remote && item.id === "local:" + workspace,
      );
      if (saved) linkTrust = saved.trust;
      else {
        const choice = await dialog.showMessageBox(win, {
          type: "question",
          message: say("Open “{folder}” from a link?").replace(
            "{folder}",
            basename(workspace!) || workspace!,
          ),
          detail:
            workspace +
            "\n\n" +
            say(
              "It opens with its project configuration off: MCP servers, hooks and extensions in this folder do not run. To turn them on, open the folder with Open project.",
            ),
          buttons: [say("Open folder"), say("Cancel")],
          defaultId: 1,
          cancelId: 1,
        });
        if (choice.response !== 0) return null;
        linkTrust = false;
      }
    }
    const serverId = url ? remoteLocationId(url) : "";
    const explicitAuth =
      input.password !== undefined ||
      input.username !== undefined ||
      input.token !== undefined;
    let auth: ServerAuth = input.password
      ? {
          type: "basic",
          username: input.username?.trim() || "haarness",
          password: input.password,
        }
      : input.token
        ? { type: "bearer", token: input.token }
        : explicitAuth
          ? { type: "none" }
          : serverAuth(serverId);
    // A local server already opened by this Desktop has private credentials;
    // reuse them on an exact origin match without asking the user to copy them.
    if (
      url &&
      auth.type === "none" &&
      ["localhost", "127.0.0.1", "[::1]"].includes(new URL(url).hostname)
    ) {
      const local = [...connections.values()].find(
        (item) =>
          !item.view.remote && new URL(item.url).origin === new URL(url).origin,
      );
      if (local) auth = local.auth;
    }

    let remoteWorkspace = recent?.remoteWorkspace;
    let existing = recent;
    if (url && input.workspace && !input.recent) {
      const opened = await openRemote(url, auth, input.workspace);
      workspace = opened.path;
      existing = data().recent.find(
        (item) =>
          item.remote &&
          item.workspace === opened.path &&
          serverKey(item) === serverId,
      );
      remoteWorkspace = existing ? existing.remoteWorkspace : opened.id;
    }
    const id = url
      ? remoteLocationId(url, remoteWorkspace)
      : "local:" + workspace;
    if (worktreeOperations.has(id))
      throw new Error("This worktree is being updated. Please retry shortly.");
    return projectTransaction(id, async () => {
      // Resolve saved policy after earlier operations have committed. A sidebar
      // refresh must not undo an explicit setting while a daemon is restarting.
      const current = data().recent.find((item) => item.id === id);
      const server = data().servers.find((item) => item.id === serverId);
      const view = url
        ? await establish(
            workspace ?? "",
            url,
            auth,
            true,
            false,
            undefined,
            false,
            remoteWorkspace,
          )
        : await connectLocal(
            workspace!,
            linkTrust ??
              input.trust ??
              (input.recent ? current?.trust : undefined) ??
              true,
          );
      const saved: Recent = {
        id: view.id,
        label: view.label,
        workspace: view.workspace,
        remote: view.remote,
        remoteWorkspace: view.remoteWorkspace,
        trust: view.trust,
        project: view.project ?? current?.project,
        checkout: view.checkout ?? current?.checkout,
        ...(url ? { url } : {}),
      };
      // A saved project removed from the sidebar while it connected (a row
      // retrying in the background, a notification opening its chat), or
      // while its folder was looked up afterwards, stays removed: nothing of
      // it is saved or watched again.
      const kept = await rememberConnected({
        recent: () => data().recent,
        saved,
        requested: input.recent,
        save: async (next) => {
          if (url) {
            saveServerAuth(serverId, auth);
            const savedServer = {
              id: serverId,
              url: new URL(url).origin,
              label:
                input.label?.trim() || server?.label || new URL(url).hostname,
            };
            const serverIndex = data().servers.findIndex(
              (item) => item.id === serverId,
            );
            if (serverIndex >= 0) data().servers[serverIndex] = savedServer;
            else data().servers.push(savedServer);
          }
          data().recent = next;
          await persist();
        },
        after: view.remote
          ? undefined
          : async () => {
              await refreshProjects();
              if (!data().recent.find((item) => item.id === view.id)?.project)
                await refreshProjects();
              const annotated = data().recent.find(
                (item) => item.id === view.id,
              );
              if (annotated)
                Object.assign(view, {
                  project: annotated.project,
                  checkout: annotated.checkout,
                });
            },
        watch: () => watchNotifications(view.id, win.id),
      });
      return kept ? view : null;
    });
  });
  handle("forget", async (_, id: string) => {
    for (const item of data().recent)
      if (item.id === id || projectKey(item) === id)
        { forgetNotifications(item.id); browserHost.disconnect(item.id); }
    data().recent = data().recent.filter(
      (item) => item.id !== id && projectKey(item) !== id,
    );
    await persist();
    broadcast({ kind: "projects", data: data().recent });
  });
  handle("server", async (_, raw: unknown) => {
    const input = z
      .object({
        id: z.string(),
        action: z.enum(["rename", "remove"]),
        label: z.string().trim().min(1).max(200).optional(),
      })
      .strict()
      .parse(raw);
    return projectTransaction(input.id, async () => {
      const server = data().servers.find((item) => item.id === input.id);
      if (!server) throw new Error("Saved server was not found");
      if (input.action === "rename") {
        if (!input.label) throw new Error("Enter a server name");
        server.label = input.label;
      } else {
        for (const item of data().recent)
          if (serverKey(item) === input.id) forgetNotifications(item.id);
        data().servers = data().servers.filter((item) => item.id !== input.id);
        data().recent = data().recent.filter(
          (item) => serverKey(item) !== input.id,
        );
        delete data().credentials[input.id];
      }
      await persist();
      broadcast({ kind: "projects", data: data().recent });
      return data().servers;
    });
  });
  handle("api", async (_, raw) => {
    const input = request.parse(raw);
    if (!apis.has(input.method)) throw new Error("Unsupported API operation");
    if (worktreeOperations.has(input.connection))
      throw new Error("This worktree is being updated. Please retry shortly.");
    if (
      ["resetWorkspace", "removeWorkspace", "cleanupWorkspace"].includes(
        input.method,
      )
    )
      return manageWorktree(input.connection, input.method, input.args);
    const client = connection(input.connection).client;
    const fn = (client as unknown as Record<string, unknown>)[input.method];
    if (typeof fn !== "function")
      throw new Error("This server operation is not available");
    const result = await fn.apply(client, input.args);
    return input.method === "listThreads"
      ? withThreadKinds(result, (id) => client.listItems(id))
      : result;
  });
  handle(
    "files",
    async (_, id: string, action: string, input: Record<string, unknown>) => {
      const item = connection(id);
      const spec = (
        {
          list: ["GET", "list"],
          read: ["GET", "content"],
          preview: ["GET", "preview"],
          previewCurrent: ["GET", "preview/current"],
          restore: ["POST", "preview/restore"],
          write: ["PUT", "content"],
          action: ["POST", "action"],
        } as Record<string, string[]>
      )[action];
      if (!spec) throw new Error("Unknown file operation");
      const url = new URL(item.url + "/v1/files/" + spec[1]);
      if (spec[0] === "GET")
        for (const [key, value] of Object.entries(input))
          url.searchParams.set(key, String(value));
      const response = await fetch(url, {
        method: spec[0],
        headers: {
          ...authorizationHeaders(item.auth),
          "Content-Type": "application/json",
        },
        body: spec[0] === "GET" ? undefined : JSON.stringify(input),
        signal: AbortSignal.timeout(30000),
        redirect: "error",
      });
      if (!response.ok) throw new Error(await response.text());
      return response.json();
    },
  );
  handle("native", async (_, id: string, input: Record<string, unknown>) => {
    const item = connection(id);
    if (item.view.remote) {
      if (
        input.action === "asset" &&
        ["agent", "command", "skill"].includes(String(input.kind))
      )
        return item.client.manageResource("assets", {
          ...(input.input as Record<string, JsonValue>),
          kind: String(input.kind),
        });
      throw new Error("Open a local project to manage this installation.");
    }
    return (
      await invoke(item.view.workspace, item.view.trust, ["desktop"], input)
    ).value;
  });
  handle("command", async (win, id: string, args: string[], task: string) => {
    z.string().uuid().parse(task);
    if (commandOwners.has(task))
      throw new Error("This operation is already running");
    commandOwners.set(task, win.id);
    try {
      return await command(id, args, task, (event) => send(win, event));
    } finally {
      commandOwners.delete(task);
    }
  });
  handle("cancel", (win, id: string) => {
    const child = commands.get(id);
    if (child && commandOwners.get(id) === win.id) stopCommand(child);
  });
  handle("subscribe", (win, id: string, cursor: number) => {
    if (!Number.isSafeInteger(cursor) || cursor < 0)
      throw new Error("Invalid event cursor");
    const item = connection(id);
    const key = randomUUID();
    // The window learns when the stream's response arrived ("opened"): only
    // a stream that stays open from then counts as working, not one a proxy
    // holds without an answer and then refuses.
    const opened = (response: Response) => {
      if (response.ok && subscriptions.has(key))
        send(win, { kind: "connection", connection: id, id: key, data: "opened" });
      return response;
    };
    const client = new Haarness({
      baseUrl: item.url,
      auth: item.auth,
      fetch: (request, init) => fetch(request, init).then(opened),
    });
    subscriptions.set(key, { owner: win.id, client });
    void (async () => {
      try {
        for await (const event of client.events({
          after: cursor,
          reconnect: false,
        })) {
          send(win, { kind: "server", connection: id, id: key, data: event });
        }
        send(win, {
          kind: "connection",
          connection: id,
          id: key,
          data: "resync",
        });
      } catch (error) {
        if (subscriptions.has(key))
          send(win, {
            kind: "connection",
            connection: id,
            id: key,
            data: error instanceof Error ? error.message : "Connection lost",
          });
      } finally {
        subscriptions.delete(key);
        client.close();
      }
    })();
    return key;
  });
  handle("unsubscribe", (win, id: string) => {
    const item = subscriptions.get(id);
    if (item?.owner === win.id) {
      subscriptions.delete(id);
      item.client.close();
    }
  });
  handle(
    "terminal",
    async (win, id: string, action: string, pty: string, input: unknown) => {
      const key = win.id + ":" + id + ":" + pty;
      if (action === "detach") {
        // Only the view's own socket: a newer view of the terminal keeps its.
        // A socket the view let go of ends without a "closed" event.
        terminals.detach(key, z.string().max(100).parse(input));
        return;
      }
      if (action === "write") {
        const value = z
          .array(z.number().int().min(0).max(255))
          .max(65536)
          .parse(input);
        // False while there is no open socket, so the view keeps the input
        // for the reconnect instead of losing it. A socket the server closed
        // (it drops a reader that fell behind) stays listed until its last
        // output is forwarded, and what is sent on it meanwhile is lost.
        const current = terminals.get(key);
        if (!current || !socketOpen(current.socket)) return false;
        current.socket.send(new Uint8Array(value));
        return true;
      }
      if (action !== "connect") throw new Error("Unknown terminal action");
      const request = z
        .object({ cursor: z.number().int().min(0), view: z.string().min(1).max(100) })
        .parse(input);
      terminals.begin(key, request.view);
      if (terminalOutage()) throw new Error("The terminal server cannot be reached");
      const socket = await connection(id).client.connectPty(pty, {
        cursor: request.cursor,
      });
      if (!terminals.accept(key, request.view, win.id, socket))
        throw new Error("Terminal connection replaced");
      const view = request.view;
      void (async () => {
        let failure: string | undefined;
        try {
          for await (const message of socket)
            send(win, {
              kind: "pty",
              connection: id,
              id: pty,
              data:
                message instanceof Uint8Array
                  ? { bytes: Array.from(message), view }
                  : { ...message, view },
            });
        } catch (error) {
          failure = String(error);
        } finally {
          // The server ends a socket whose reader fell behind, and a network
          // or server outage ends it too. Unless the view detached or
          // connected again, tell it, so it reconnects from its cursor.
          if (terminals.ended(key, socket)) {
            send(win, {
              kind: "pty",
              connection: id,
              id: pty,
              data: { type: "closed", view, ...(failure ? { error: failure } : {}) },
            });
          }
        }
      })();
    },
  );
  handle("pick", async (win, kind: string) => {
    if (!["file", "directory"].includes(kind))
      throw new Error("Invalid picker");
    const result = await dialog.showOpenDialog(win, {
      properties:
        kind === "directory"
          ? ["openDirectory", "createDirectory"]
          : ["openFile"],
    });
    return result.canceled ? null : result.filePaths[0];
  });
  handle("attachments", async (win) => {
    const result = await dialog.showOpenDialog(win, {
      title: say("Attach files"),
      properties: ["openFile", "multiSelections"],
      filters: [
        {
          name: say("Media and documents"),
          extensions: [
            "png",
            "jpg",
            "jpeg",
            "webp",
            "gif",
            "heic",
            "heif",
            "bmp",
            "tif",
            "tiff",
            "pdf",
            "mp3",
            "wav",
            "mp4",
            "webm",
            "m4a",
            "ogg",
            "flac",
            "mov",
          ],
        },
      ],
    });
    if (result.canceled) return [];
    const values = await attachments(result.filePaths);
    return values.map((data, i) => ({
      data,
      name: basename(result.filePaths[i]),
    }));
  });
  // The composer's Attach menu opened: a token its Attach screenshots choice lists with.
  handle("screenshotMenu", (win) => screenshotIntents.menu(win.id));
  // The Screenshots dialog: the newest images of the screenshots folder, with
  // thumbnails. The renderer gets names, times and thumbnails, never paths,
  // and attaches by place in this listing.
  handle("screenshots", async (win, raw: unknown) => {
    const input = z
      .object({ choose: z.boolean().optional(), token: z.string().max(100).optional() })
      .strict()
      .parse(raw ?? {});
    let folder: string | undefined;
    if (input.choose) {
      const result = await dialog.showOpenDialog(win, {
        title: say("Choose your screenshots folder"),
        properties: ["openDirectory"],
      });
      if (result.canceled || !result.filePaths[0]) return { canceled: true };
      folder = result.filePaths[0];
      // Linux has no screenshots folder of its own: the picker opens the
      // chosen one next time. Elsewhere it opens the system's folder again.
      if (process.platform === "linux") {
        data().screenshotFolder = folder;
        await persist();
      }
    } else {
      // The person chose Attach screenshots in the Attach menu, with presses
      // main saw on their way to this window. Otherwise (VoiceOver's VO-Space
      // and Voice Control press a control without either) Desktop asks
      // first, so a page never lists the folder unseen.
      if (!screenshotIntents.chosen(win.id, input.token)) {
        const choice = await dialog.showMessageBox(win, {
          type: "question",
          message: say("Show your newest screenshots?"),
          detail: say(
            "Haarness shows previews of the newest images in your screenshots folder so you can pick some to attach.",
          ),
          buttons: [say("Show screenshots"), say("Cancel")],
          defaultId: 0,
          cancelId: 1,
        });
        if (choice.response !== 0) return { canceled: true };
      }
      folder = await defaultScreenshotFolder(data().screenshotFolder);
    }
    if (!folder) {
      screenshotListings.forget(win.id);
      return { needsFolder: true };
    }
    const images = await listScreenshots(folder);
    const thumbnails = await screenshotThumbnails(images);
    return {
      folder: displayFolder(folder, homedir()),
      listing: screenshotListings.remember(
        win.id,
        images.map(({ path, identity }) => ({ path, identity })),
      ),
      items: images.map((image, index) => ({
        name: image.name,
        modified: image.modified,
        thumbnail: thumbnails[index],
      })),
    };
  });
  handle("screenshotAttach", async (win, listing: unknown, indexes: unknown) => {
    try {
      const picks = screenshotListings.resolve(win.id, listing, indexes);
      // Only files that are still the ones listed: a name swapped for a link
      // or another file since is refused, never followed.
      const values = await readListedScreenshots(picks);
      // A listing attaches once: another pick needs the dialog opened again.
      screenshotListings.forget(win.id);
      return values.map((data, index) => ({ data, name: basename(picks[index].path) }));
    } catch (error) {
      throw new Error(say(error instanceof Error ? error.message : String(error)));
    }
  });
  handle("save", async (win, name: string, content: string) => {
    bounded(content);
    const result = await dialog.showSaveDialog(win, {
      defaultPath: basename(name),
    });
    if (result.canceled || !result.filePath) return false;
    await writeFile(result.filePath, content, { mode: 0o600 });
    return true;
  });
  handle("openArchive", async (win) => {
    const result = await dialog.showOpenDialog(win, {
      filters: [{ name: say("Chat archive"), extensions: ["json"] }],
      properties: ["openFile"],
    });
    if (result.canceled) return null;
    const path = result.filePaths[0];
    if ((await stat(path)).size > 64 * 1024 * 1024)
      throw new Error("Archive exceeds 64 MiB");
    return JSON.parse(await readFile(path, "utf8"));
  });
  handle("windowAction", (win, action: string) => {
    if (action === "new") {
      createWindow();
      return;
    }
    if (action === "fullscreen") {
      win.setFullScreen(!win.isFullScreen());
      return;
    }
    if (action === "devtools") {
      win.webContents.toggleDevTools();
      return;
    }
    if (action === "zoom-reset") {
      win.webContents.setZoomLevel(0);
      return;
    }
    if (action === "zoom-in" || action === "zoom-out") {
      win.webContents.setZoomLevel(
        Math.max(
          -5,
          Math.min(
            5,
            win.webContents.getZoomLevel() +
              (action === "zoom-in" ? 0.5 : -0.5),
          ),
        ),
      );
      return;
    }
    throw new Error("Unknown window action");
  });
  handle("external", (_, url: string) => shell.openExternal(external(url)));
  // Haarness for Chrome is set up on this computer, whichever project is open.
  handle("chromeSetup", (_, action: unknown) =>
    chromeSetup(z.enum(["setup", "status"]).parse(action)),
  );
  handle("chromeReveal", () => revealChromeExtension());
  // An image a reply links to on another host, once the person asks for it.
  // End-to-end tests pin fixture host names with `haarnessImageHosts`.
  handle("remoteImage", (_, raw: unknown) =>
    fetchRemoteImage(raw, {
      hosts:
        process.env.HAARNESS_DESKTOP_TEST === "1"
          ? ((globalThis as { haarnessImageHosts?: Record<string, string> }).haarnessImageHosts ?? {})
          : {},
    }),
  );
  handle("openLink", async (win, id: string | undefined, thread: string | undefined, raw: string) => {
    const url = external(raw);
    if (id && browserLinkDestination(url, data().preferences) === "browser") {
      await browserHost.command(win, id, thread ?? "", { type: "tabs.open", url });
      return "browser";
    }
    await shell.openExternal(url);
    return "external";
  });
  handle("browserData", async (win, id: string, raw: unknown = { action: "get" }) => {
    connection(id);
    const input = z.object({ action: z.enum(["get", "remove", "clear", "save"]), kind: z.enum(["history", "downloads", "all"]).optional(), id: z.string().max(128).optional() }).parse(raw);
    if (input.action === "save") {
      if (!input.id) throw new Error("Choose a download");
      await saveBrowserDownload(win, id, input.id);
    } else if (input.action === "remove" || input.action === "clear") {
      if (!input.kind || (input.action === "remove" && !input.id)) throw new Error("Choose browsing records to remove");
      if (input.kind === "all") await browserHost.clear(id);
      await removeBrowserData(id, input.kind, input.action === "remove" ? input.id : undefined);
    }
    return browserData(id);
  });
  handle("reveal", async (_, id: string, path: string = "") => {
    // A saved project can be revealed before it has been opened this session.
    const view =
      connections.get(id)?.view ?? data().recent.find((item) => item.id === id);
    if (!view) throw new Error("Saved project was not found");
    if (view.remote) throw new Error("This path belongs to a remote server");
    const root = await realpath(view.workspace);
    const target = await realpath(resolve(root, path));
    if (target !== root && !target.startsWith(root + sep))
      throw new Error("Path escapes workspace");
    shell.showItemInFolder(target);
  });
  handle("update", async (_, action: string) => {
    if (!process.env.HAARNESS_UPDATE_URL)
      throw new Error(
        "This build has no configured update feed. Install a newer signed release manually.",
      );
    const { autoUpdater } = await import("electron-updater");
    if (action === "install") {
      if (!updateReady)
        throw new Error("Download the update before installing.");
      autoUpdater.quitAndInstall();
      return;
    }
    if (!updateListeners) {
      updateListeners = true;
      autoUpdater.on("download-progress", (value) =>
        broadcast({
          kind: "update",
          data: { status: "downloading", percent: value.percent },
        }),
      );
      autoUpdater.on("update-downloaded", (value) => {
        updateReady = true;
        broadcast({
          kind: "update",
          data: { status: "ready", version: value.version },
        });
      });
      autoUpdater.on("error", (error) =>
        broadcast({
          kind: "update",
          data: { status: "error", message: error.message },
        }),
      );
    }
    if (action === "download") {
      if (!updateAvailable)
        throw new Error("Check for an available update first.");
      await autoUpdater.downloadUpdate();
      return { status: "ready" };
    }
    if (action !== "check") throw new Error("Unknown update operation");
    const url = new URL(process.env.HAARNESS_UPDATE_URL);
    if (url.protocol !== "https:")
      throw new Error("Update feeds require HTTPS");
    autoUpdater.setFeedURL({ provider: "generic", url: url.href });
    autoUpdater.autoDownload = false;
    const result = await autoUpdater.checkForUpdates();
    updateAvailable = result?.isUpdateAvailable ?? false;
    return { available: updateAvailable, info: result?.updateInfo };
  });
}
/**
 * The system-wide shortcut (Settings → Keyboard shortcuts, off by default):
 * from another app it brings Haarness's last window to the front, and with
 * Haarness in front it hides it again (minimizes it outside macOS).
 */
let registeredShortcut = "";
/** The window whose Settings is recording a new global shortcut. */
let shortcutSuspendedBy: number | undefined;
/** Recording ends with the window that started it (closed, reloaded or
 * crashed), so the shortcut never stays off. */
function resumeShortcut(win: number) {
  if (shortcutSuspendedBy !== win) return;
  shortcutSuspendedBy = undefined;
  registerShortcut();
}
function toggleFromShortcut() {
  const focused = BrowserWindow.getFocusedWindow();
  if (focused && windows.has(focused)) {
    if (process.platform === "darwin") app.hide();
    else focused.minimize();
    return;
  }
  const win = [...windows].at(-1);
  if (!win) return void createWindow();
  if (win.isMinimized()) win.restore();
  win.show();
  win.focus();
  if (process.platform === "darwin") app.focus({ steal: true });
}
/** Registers `value` (the saved one by default) in place of the current one;
 * false when another app or the system holds it, with nothing registered. */
function registerShortcut(value: GlobalShortcut = data().preferences.globalShortcut) {
  if (registeredShortcut) globalShortcut.unregister(registeredShortcut);
  registeredShortcut = "";
  const accelerator = value.enabled ? globalAccelerator(value.keys) : undefined;
  if (!accelerator) return true;
  try {
    if (!globalShortcut.register(accelerator, toggleFromShortcut)) return false;
  } catch {
    return false;
  }
  // While Settings records a new one, a free combination is only tried.
  if (shortcutSuspendedBy !== undefined) globalShortcut.unregister(accelerator);
  else registeredShortcut = accelerator;
  return true;
}
/**
 * The saved theme's page colour. The window paints it before its page draws
 * anything, and the page leaves it showing until it applies the theme
 * (base.css), so a light theme never shows a dark first frame, nor a dark one
 * a light frame.
 */
function windowBackground() {
  const { theme, palette } = data().preferences;
  const dark = theme === "dark" || (theme === "system" && nativeTheme.shouldUseDarkColors);
  return themeColors(palette, dark ? "dark" : "light").page;
}
function repaintWindows() {
  for (const win of windows) if (!win.isDestroyed()) win.setBackgroundColor(windowBackground());
}
function createWindow() {
  if (!initialized || quitting) return;
  const state = data().values.window as
    | { width?: number; height?: number }
    | undefined;
  const win = new BrowserWindow({
    // Test runs keep windows hidden, except on Linux: X11 (and Xvfb in CI)
    // draws no frames for a window that was never shown, so every screenshot
    // and visual check would wait forever.
    show: process.env.HAARNESS_DESKTOP_TEST !== "1" || process.platform === "linux",
    width: state?.width ?? 1420,
    height: state?.height ?? 940,
    minWidth: 900,
    minHeight: 640,
    title: "Haarness",
    backgroundColor: windowBackground(),
    titleBarStyle: process.platform === "darwin" ? "hiddenInset" : "default",
    trafficLightPosition: { x: 18, y: 17 },
    webPreferences: {
      preload,
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      spellcheck: true,
    },
  });
  windows.add(win);
  win.webContents.setWindowOpenHandler(({ url }) => {
    void shell.openExternal(external(url)).catch(() => {});
    return { action: "deny" };
  });
  win.webContents.on("will-navigate", (event, url) => {
    if (!source(url)) event.preventDefault();
  });
  win.webContents.on("input-event", (_event, input) => {
    const kind = inputKind(input);
    if (kind) screenshotIntents.input(win.id, kind);
  });
  win.webContents.session.setPermissionRequestHandler(
    (_web, permission, callback) =>
      callback(permission === "clipboard-sanitized-write"),
  );
  win.webContents.on("will-prevent-unload", (event) => {
    const choice = dialog.showMessageBoxSync(win, {
      type: "warning",
      buttons: [say("Keep editing"), say("Discard and close")],
      defaultId: 0,
      cancelId: 0,
      message: say("There are unsaved file edits."),
      detail: say("Save your files before closing, or discard the edits."),
    });
    if (choice === 1) event.preventDefault();
    else quitting = false;
  });
  win.webContents.on(
    "did-start-navigation",
    (_event, _url, _inPlace, mainFrame) => {
      if (mainFrame) {
        voice.stop(win.id);
        notificationReady.delete(win.id);
      }
    },
  );
  win.webContents.on("render-process-gone", () => {
    resumeShortcut(win.id);
    voice.stop(win.id);
    if (!quitting && !win.isDestroyed())
      void dialog
        .showMessageBox(win, {
          type: "error",
          message: say("The window stopped responding."),
          detail: say("Your tasks keep running when you close this window."),
          buttons: [say("Reload window"), say("Close")],
        })
        .then((result) => {
          if (quitting || win.isDestroyed()) return;
          if (result.response === 0) win.reload();
          else win.close();
        });
  });
  win.on("close", () => {
    const [width, height] = win.getSize();
    data().values.window = { width, height };
    void persist();
  });
  win.webContents.on("did-start-navigation", () => {
    browserHost.hide(win);
    resumeShortcut(win.id);
  });
  win.on("closed", () => {
    screenshotListings.forget(win.id);
    screenshotIntents.forget(win.id);
    resumeShortcut(win.id);
    voice.stop(win.id);
    windows.delete(win);
    notificationReady.delete(win.id);
    notificationPending.delete(win.id);
    for (const [id, item] of subscriptions)
      if (item.owner === win.id) {
        subscriptions.delete(id);
        item.client.close();
      }
    terminals.close(win.id);
  });
  if (process.env.ELECTRON_RENDERER_URL)
    void win.loadURL(process.env.ELECTRON_RENDERER_URL);
  else void win.loadFile(renderer);
  win.webContents.once("did-finish-load", () => {
    if (pendingLink) {
      send(win, {
        kind: "menu",
        data: { action: "deep-link", url: pendingLink },
      });
      pendingLink = undefined;
    }
  });
  return win;
}
function menu() {
  const t = (key: string) => translate(data().preferences.language, key);
  const action = (name: string) => {
    const win = BrowserWindow.getFocusedWindow();
    if (win) send(win, { kind: "menu", data: name });
  };
  Menu.setApplicationMenu(
    Menu.buildFromTemplate([
      ...(process.platform === "darwin"
        ? [
            {
              label: t("Haarness"),
              submenu: [
                { label: t("About"), role: "about" as const },
                {
                  label: t("Settings") + "…",
                  accelerator: data().preferences.shortcuts.settings.replace(
                    /\bMod\b/g,
                    "CmdOrCtrl",
                  ),
                  click: () => action("settings"),
                },
                { type: "separator" as const },
                { label: t("Hide Haarness"), role: "hide" as const },
                { label: t("Hide others"), role: "hideOthers" as const },
                { label: t("Show all"), role: "unhide" as const },
                { type: "separator" as const },
                { label: t("Quit Haarness"), role: "quit" as const },
              ],
            },
          ]
        : []),
      {
        label: t("File"),
        submenu: [
          {
            label: t("New chat"),
            accelerator: data().preferences.shortcuts.new.replace(
              /\bMod\b/g,
              "CmdOrCtrl",
            ),
            click: () => action("new"),
          },
          {
            label: t("Open project") + "…",
            accelerator: "CmdOrCtrl+O",
            click: () => action("open"),
          },
          {
            label: t("New window"),
            accelerator: "CmdOrCtrl+Shift+N",
            click: () => createWindow(),
          },
          { type: "separator" },
          { label: t("Close"), role: "close" },
        ],
      },
      {
        label: t("Edit"),
        submenu: [
          { label: t("Undo"), role: "undo" },
          { label: t("Redo"), role: "redo" },
          { type: "separator" },
          { label: t("Cut"), role: "cut" },
          { label: t("Copy"), role: "copy" },
          { label: t("Paste"), role: "paste" },
          { label: t("Select all"), role: "selectAll" },
        ],
      },
      {
        label: t("View"),
        submenu: [
          {
            label: t("Command palette"),
            accelerator: data().preferences.shortcuts.palette.replace(
              /\bMod\b/g,
              "CmdOrCtrl",
            ),
            click: () => action("palette"),
          },
          {
            label: t("Files"),
            accelerator: data().preferences.shortcuts.files.replace(
              /\bMod\b/g,
              "CmdOrCtrl",
            ),
            click: () => action("files"),
          },
          {
            label: t("Changes"),
            accelerator: data().preferences.shortcuts.diff.replace(
              /\bMod\b/g,
              "CmdOrCtrl",
            ),
            click: () => action("diff"),
          },
          {
            label: t("Terminal"),
            accelerator: data().preferences.shortcuts.terminal.replace(
              /\bMod\b/g,
              "CmdOrCtrl",
            ),
            click: () => action("terminal"),
          },
          {
            label: t("Browser"),
            accelerator: data().preferences.shortcuts.browser.replace(
              /\bMod\b/g,
              "CmdOrCtrl",
            ),
            click: () => action("browser"),
          },
          { type: "separator" },
          { label: t("Actual size"), role: "resetZoom" },
          { label: t("Zoom in"), role: "zoomIn" },
          { label: t("Zoom out"), role: "zoomOut" },
          { label: t("Toggle full screen"), role: "togglefullscreen" },
          { label: t("Developer tools"), role: "toggleDevTools" },
        ],
      },
      { label: t("Window"), role: "windowMenu" },
      {
        label: t("Help"),
        submenu: [
          { label: t("Keyboard shortcuts"), click: () => action("shortcuts") },
          { label: t("About Haarness"), click: () => action("about") },
        ],
      },
    ]),
  );
}
if (!process.env.HAARNESS_DESKTOP_TEST && !app.requestSingleInstanceLock())
  app.quit();
else {
  app.on("second-instance", (_, args) => {
    const url = args.find((arg) => arg.startsWith("haarness://"));
    if (url) pendingLink = url;
    const win = [...windows][0];
    if (win) {
      win.show();
      win.focus();
      if (url) send(win, { kind: "menu", data: { action: "deep-link", url } });
    }
  });
  app.on("open-url", (event, url) => {
    event.preventDefault();
    pendingLink = url;
    const win = [...windows][0];
    if (win) send(win, { kind: "menu", data: { action: "deep-link", url } });
  });
  void app
    .whenReady()
    .then(async () => {
      if (quitting) return;
      if (!app.isPackaged && !process.env.ELECTRON_RENDERER_URL) {
        const session = await createUiSession(resolve(here, ".."));
        if (quitting) {
          session.dispose();
          return;
        }
        uiSession = session;
        renderer = uiSession.renderer;
        preload = uiSession.preload;
      }
      await load();
      if (quitting) return;
      installHandlers();
      startBillingWatch(state => broadcast({ kind: "menu", data: { action: "account-billing", state } }));
      menu();
      registerShortcut();
      initialized = true;
      createWindow();
      void refreshProjects().catch(() => {});
      setupChromeOnLaunch();
      if (app.isPackaged) app.setAsDefaultProtocolClient("haarness");
      powerMonitor.on("resume", () =>
        broadcast({ kind: "connection", data: "resume" }),
      );
      nativeTheme.on("updated", () => {
        repaintWindows();
        broadcast({ kind: "menu", data: "theme" });
      });
    })
    .catch((error) => {
      if (quitting) return;
      dialog.showErrorBox("Haarness could not start", String(error));
      app.quit();
    });
  app.on("activate", () => {
    if (!windows.size) createWindow();
  });
  app.on("window-all-closed", () => {
    if (process.platform !== "darwin") app.quit();
  });
  app.on("will-quit", () => {
    globalShortcut.unregisterAll();
    uiSession?.dispose();
  });
  app.on("before-quit", () => {
    stopBillingWatch();
    browserHost.dispose();
    notifications.dispose();
    for (const watch of notificationWatches.values()) watch.stop();
    voice.stop();
    for (const item of subscriptions.values()) item.client.close();
    terminals.closeAll();
    for (const child of commands.values()) stopCommand(child);
  });
}
