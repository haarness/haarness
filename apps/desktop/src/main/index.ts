import { createReviewStorage, type ReviewRequest } from "../review-drafts";
import { browserLinkDestination } from "../browser-settings";
import { browserData, removeBrowserData, saveBrowserDownload } from "./browser-data";
import { createBrowserHost } from "./browser-host";
import { createAttachmentDraftStorage } from "./attachment-drafts";
import { translate } from "../localization";
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
} from "electron";
import { readFile, writeFile, realpath, stat } from "node:fs/promises";
import { join, resolve, basename, sep } from "node:path";
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
import type { DesktopEvent, Recent, NotificationTarget } from "../shared";
import { serverKey } from "../servers";
import { mergeProjectCatalog, projectKey } from "../projects";
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
  preferences,
  serverAuth,
  saveServerAuth,
} from "./store";
import { bounded, external, request } from "./validation";
import { createVoiceInput, launchVoice } from "./voice";
import { createProjectQueue } from "./project-queue";
import { createUiSession } from "./ui-session";
import { createNotifications } from "./notifications";

if (process.env.HAARNESS_DESKTOP_DATA)
  app.setPath("userData", resolve(process.env.HAARNESS_DESKTOP_DATA));
app.setName("Haarness");
// A development run boots the stock Electron bundle, which brings its own Dock
// icon. Point it at ours so the app looks like itself before it is packaged;
// a packaged build gets the icon from the bundle instead.
if (!app.isPackaged && process.platform === "darwin")
  app.whenReady().then(() => {
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
const commandOwners = new Map<string, number>();
const terminals = new Map<string, { owner: number; socket: PtyConnection }>();
const projectTransaction = createProjectQueue();
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
let pendingLink: string | undefined;
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
  open: (target, owner) => {
    const win =
      [...windows].find((win) => win.id === owner) ??
      [...windows][0] ??
      createWindow();
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
          .receive(id, item.view.label, watch.owner, event, (thread) =>
            client.getThread(thread),
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
  `manageResource info analyticsStats analyticsInsights providers refreshProviders agents commands lspStatus formatterStatus toolCatalog updateLazyTools updateTool projects workspacesList createWorkspace workspaceRefs resolveWorkspaceRef getWorkspace removeWorkspace openWorkspace resetWorkspace cleanupWorkspace listPtys createPty getPty updatePty deletePty workflows workflowSource startWorkflow listWorkflowRuns getWorkflowRun deleteWorkflowRun waitWorkflowRun cancelWorkflowRun pauseWorkflowRun resumeWorkflowRun skills loadSkill plugins pluginAction mcpServers configureMcp reconnectMcp disconnectMcp mcpTools callMcpTool mcpResources mcpResourceTemplates readMcpResource mcpPrompts getMcpPrompt listThreads createThread getThread updateThread exportSessionArchive forkThread listItems listTodos activeTurn interruptThreadTree listBackgroundTasks promoteBackgroundTasks pendingQuestion startTurn executeCommand queueInput inputQueue changeInputQueue btw compact getGoal setGoal getLoop setLoop threadUsage revertThread unrevertThread lastTurnDiff lastTurnReview vcsDiff searchFiles importSessionArchive activeTurns getTurn interruptTurn waitTurn listPermissions replyPermission listQuestions replyQuestion shutdownServer health openapi listProfiles uploadProfile getProfile deleteProfile downloadProfile`.split(
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
    const value = await preferences(input);
    menu();
    broadcast({ kind: "menu", data: { action: "preferences", value } });
    return value;
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
        throw new Error("The review belongs to another project or conversation");
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
  handle("connect", async (win, raw: unknown) => {
    const input = z
      .object({
        workspace: z.string().optional(),
        url: z.string().optional(),
        token: z.string().optional(),
        username: z.string().max(1024).optional(),
        password: z.string().max(8192).optional(),
        label: z.string().optional(),
        trust: z.boolean().optional(),
        recent: z.string().optional(),
      })
      .strict()
      .parse(raw);
    const recent = input.recent
      ? data().recent.find((item) => item.id === input.recent)
      : undefined;
    if (input.recent && !recent) throw new Error("Saved project was not found");
    let workspace = input.workspace ?? recent?.workspace;
    const url = input.url ?? recent?.url;
    if (!url && !workspace) {
      const result = await dialog.showOpenDialog(win, {
        title: "Open project",
        properties: ["openDirectory", "createDirectory"],
      });
      if (result.canceled) return null;
      workspace = result.filePaths[0];
    }
    if (!url) workspace = await realpath(workspace!);
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
            input.trust ?? (input.recent ? current?.trust : undefined) ?? true,
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
      if (url) {
        saveServerAuth(serverId, auth);
        const savedServer = {
          id: serverId,
          url: new URL(url).origin,
          label: input.label?.trim() || server?.label || new URL(url).hostname,
        };
        const serverIndex = data().servers.findIndex(
          (item) => item.id === serverId,
        );
        if (serverIndex >= 0) data().servers[serverIndex] = savedServer;
        else data().servers.push(savedServer);
      }
      const index = data().recent.findIndex((item) => item.id === view.id);
      if (index >= 0) data().recent[index] = saved;
      else data().recent.push(saved);
      await persist();
      if (!view.remote) {
        await refreshProjects();
        if (!data().recent.find((item) => item.id === view.id)?.project)
          await refreshProjects();
        const annotated = data().recent.find((item) => item.id === view.id);
        if (annotated)
          Object.assign(view, {
            project: annotated.project,
            checkout: annotated.checkout,
          });
      }
      watchNotifications(view.id, win.id);
      return view;
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
    const client = new Haarness({ baseUrl: item.url, auth: item.auth });
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
        terminals.get(key)?.socket.close();
        terminals.delete(key);
        return;
      }
      if (action === "write") {
        const value = z
          .array(z.number().int().min(0).max(255))
          .max(65536)
          .parse(input);
        await terminals.get(key)?.socket.send(new Uint8Array(value));
        return;
      }
      if (action !== "connect") throw new Error("Unknown terminal action");
      terminals.get(key)?.socket.close();
      const socket = await connection(id).client.connectPty(pty, {
        cursor: typeof input === "number" ? input : 0,
      });
      terminals.set(key, { owner: win.id, socket });
      void (async () => {
        try {
          for await (const message of socket)
            send(win, {
              kind: "pty",
              connection: id,
              id: pty,
              data:
                message instanceof Uint8Array
                  ? { bytes: Array.from(message) }
                  : message,
            });
        } catch (error) {
          send(win, {
            kind: "pty",
            connection: id,
            id: pty,
            data: { error: String(error) },
          });
        } finally {
          if (terminals.get(key)?.socket === socket) terminals.delete(key);
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
      title: "Attach files",
      properties: ["openFile", "multiSelections"],
      filters: [
        {
          name: "Media and documents",
          extensions: [
            "png",
            "jpg",
            "jpeg",
            "webp",
            "gif",
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
      filters: [{ name: "Session archive", extensions: ["json"] }],
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
function createWindow() {
  const state = data().values.window as
    | { width?: number; height?: number }
    | undefined;
  const win = new BrowserWindow({
    show: process.env.HAARNESS_DESKTOP_TEST !== "1",
    width: state?.width ?? 1420,
    height: state?.height ?? 940,
    minWidth: 900,
    minHeight: 640,
    title: "Haarness",
    backgroundColor: "#151413",
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
  win.webContents.session.setPermissionRequestHandler(
    (_web, permission, callback) =>
      callback(permission === "clipboard-sanitized-write"),
  );
  win.webContents.on("will-prevent-unload", (event) => {
    const choice = dialog.showMessageBoxSync(win, {
      type: "warning",
      buttons: ["Keep editing", "Discard and close"],
      defaultId: 0,
      cancelId: 0,
      message: "There are unsaved file edits.",
      detail: "Save your files before closing, or discard the edits.",
    });
    if (choice === 1) event.preventDefault();
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
    voice.stop(win.id);
    if (!win.isDestroyed())
      void dialog
        .showMessageBox(win, {
          type: "error",
          message: "The window stopped responding.",
          detail: "Your tasks keep running when you close this window.",
          buttons: ["Reload window", "Close"],
        })
        .then((result) => (result.response === 0 ? win.reload() : win.close()));
  });
  win.on("close", () => {
    const [width, height] = win.getSize();
    data().values.window = { width, height };
    void persist();
  });
  win.webContents.on("did-start-navigation", () => browserHost.hide(win));
  win.on("closed", () => {
    voice.stop(win.id);
    windows.delete(win);
    notificationReady.delete(win.id);
    notificationPending.delete(win.id);
    for (const [id, item] of subscriptions)
      if (item.owner === win.id) {
        subscriptions.delete(id);
        item.client.close();
      }
    for (const [id, item] of terminals)
      if (item.owner === win.id) {
        item.socket.close();
        terminals.delete(id);
      }
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
            label: t("New session"),
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
      if (!app.isPackaged && !process.env.ELECTRON_RENDERER_URL) {
        uiSession = await createUiSession(resolve(here, ".."));
        renderer = uiSession.renderer;
        preload = uiSession.preload;
      }
      await load();
      installHandlers();
      menu();
      createWindow();
      void refreshProjects().catch(() => {});
      if (app.isPackaged) app.setAsDefaultProtocolClient("haarness");
      powerMonitor.on("resume", () =>
        broadcast({ kind: "connection", data: "resume" }),
      );
      nativeTheme.on("updated", () =>
        broadcast({ kind: "menu", data: "theme" }),
      );
    })
    .catch((error) => {
      dialog.showErrorBox("Haarness could not start", String(error));
      app.quit();
    });
  app.on("activate", () => {
    if (!windows.size) createWindow();
  });
  app.on("window-all-closed", () => {
    if (process.platform !== "darwin") app.quit();
  });
  app.on("will-quit", () => uiSession?.dispose());
  app.on("before-quit", () => {
    browserHost.dispose();
    notifications.dispose();
    for (const watch of notificationWatches.values()) watch.stop();
    voice.stop();
    for (const item of subscriptions.values()) item.client.close();
    for (const item of terminals.values()) item.socket.close();
    for (const child of commands.values()) stopCommand(child);
  });
}
