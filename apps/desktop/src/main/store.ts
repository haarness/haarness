import { displayLanguage, parseLanguage, translate } from "../localization";
import type { ServerAuth } from "@haarness/sdk";
import { z } from "zod";
import { app, safeStorage } from "electron";
import { mkdir, readFile, writeFile, rename } from "node:fs/promises";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import {
  defaults,
  defaultShortcuts,
  migrateAgentKeys,
  migrateTabKeys,
  savedThinking,
  shortcutAccepted,
  shortcutsClash,
  type Preferences,
  type Recent,
  type SavedServer,
} from "../shared";
import { migrateCloudSessions, migrateProjectlessLocations } from "../projects";
import { migrateServers } from "../servers";
import { themeIds } from "../themes";
import { globalShortcutError } from "../global-shortcut";
interface Data {
  preferences: Preferences;
  recent: Recent[];
  servers: SavedServer[];
  values: Record<string, unknown>;
  credentials: Record<string, string>;
  /** Set once the old Tab agent keys have moved to the current defaults. */
  agentKeysMigrated?: boolean;
  /** The screenshots folder chosen on Linux, where the picker opens next
   * time. Kept outside `values`, which the renderer may write. */
  screenshotFolder?: string;
}
let state: Data = {
  preferences: { ...defaults, shortcuts: defaultShortcuts(process.platform) },
  recent: [],
  servers: [],
  values: {},
  credentials: {},
};
let chain = Promise.resolve();
const path = () => join(app.getPath("userData"), "desktop.json");
export async function load() {
  let saved: Record<string, string> | undefined;
  try {
    const raw = JSON.parse(await readFile(path(), "utf8"));
    saved = raw.preferences?.shortcuts;
    const { showReasoning: _legacy, ...preferences } = raw.preferences ?? {};
    state = {
      ...state,
      ...raw,
      preferences: { ...defaults, ...preferences, shortcuts: { ...defaultShortcuts(process.platform), ...raw.preferences?.shortcuts }, globalShortcut: { ...defaults.globalShortcut, ...raw.preferences?.globalShortcut }, language: displayLanguage(raw.preferences?.language), thinking: savedThinking(raw.preferences) },
    };
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT")
      throw new Error(
        "Desktop preferences could not be read. Preserve desktop.json and repair it before continuing.",
      );
  }
  const recent = migrateCloudSessions(migrateProjectlessLocations(state.recent));
  const servers = migrateServers(recent, state.servers);
  const shortcuts = migrateTabKeys(
    saved,
    state.agentKeysMigrated
      ? state.preferences.shortcuts
      : migrateAgentKeys(state.preferences.shortcuts),
    process.platform,
    state.preferences.globalShortcut.enabled ? state.preferences.globalShortcut.keys : undefined,
  );
  state.agentKeysMigrated = true;
  if (
    recent !== state.recent ||
    JSON.stringify(servers) !== JSON.stringify(state.servers) ||
    shortcuts !== state.preferences.shortcuts
  ) {
    state.servers = servers;
    state.recent = recent;
    state.preferences = { ...state.preferences, shortcuts };
    await persist();
  }
  return state;
}
export function data() {
  return state;
}
/** Settles once every write asked for so far has finished. */
export function written() {
  return chain.catch(() => {});
}
export function persist() {
  chain = chain
    .catch(() => {})
    .then(async () => {
      await mkdir(app.getPath("userData"), { recursive: true });
      const temporary = path() + "." + randomUUID();
      await writeFile(temporary, JSON.stringify(state), { mode: 0o600 });
      await rename(temporary, path());
    });
  return chain;
}
function credential(id: string) {
  const encoded = state.credentials[id];
  return encoded
    ? safeStorage.decryptString(Buffer.from(encoded, "base64"))
    : "";
}
export function serverAuth(id: string): ServerAuth {
  const value = credential(id);
  if (!value) return { type: "none" };
  // Legacy installations stored a bare bearer token in encrypted storage.
  if (!value.startsWith("haarness-auth:v1:"))
    return { type: "bearer", token: value };
  return z
    .discriminatedUnion("type", [
      z.object({ type: z.literal("none") }),
      z.object({ type: z.literal("bearer"), token: z.string() }),
      z.object({
        type: z.literal("basic"),
        username: z.string(),
        password: z.string(),
      }),
    ])
    .parse(JSON.parse(value.slice("haarness-auth:v1:".length)));
}
export function saveServerAuth(id: string, auth: ServerAuth) {
  setCredential(
    id,
    auth.type === "none" ? "" : "haarness-auth:v1:" + JSON.stringify(auth),
  );
}
function setCredential(id: string, token: string) {
  if (!token) {
    delete state.credentials[id];
    return;
  }
  if (
    !safeStorage.isEncryptionAvailable() ||
    safeStorage.getSelectedStorageBackend?.() === "basic_text"
  )
    throw new Error(
      "Secure credential storage is unavailable. Configure the operating system keyring before saving a remote server.",
    );
  state.credentials[id] = safeStorage.encryptString(token).toString("base64");
}
export async function preferences(input: Partial<Preferences>) {
  const candidate = z
    .object({
      theme: z.enum(["dark", "light", "system"]),
      palette: z.enum(themeIds),
      fontSize: z.number().int().min(11).max(24),
      terminalFontSize: z.number().int().min(10).max(24),
      terminalOptionAsMeta: z.enum(["off", "left", "both"]),
      notifications: z.boolean(),
      completionNotifications: z.enum(["background", "always", "never"]),
      permissionNotifications: z.boolean(),
      questionNotifications: z.boolean(),
      browserWebLinks: z.enum(["external", "browser"]),
      browserLocalLinks: z.enum(["external", "browser"]),
      browserShowFullURL: z.boolean(),
      browserAskDownloadLocation: z.boolean(),
      browserAgentCursor: z.boolean(),
      sound: z.boolean(),
      compactTools: z.boolean(),
      thinking: z.enum(["compact", "show", "hide"]),
      sendKey: z.enum(["enter", "mod-enter"]),
      editorWrap: z.boolean(),
      diffLayout: z.enum(["split", "unified"]),
      language: z.string().refine((value) => !!parseLanguage(value), "Unsupported display language").transform((value) => displayLanguage(value)),
      shortcuts: z.object({
        new: z.string(),
        settings: z.string(),
        palette: z.string(),
        files: z.string(),
        diff: z.string(),
        terminal: z.string(),
        browser: z.string(),
        pick: z.string(),
        sideChat: z.string(),
        agentNext: z.string(),
        agentPrev: z.string(),
        approvalNext: z.string(),
        tabNext: z.string(),
        tabPrev: z.string(),
        chatPrev: z.string(),
        chatNext: z.string(),
        focusSidebar: z.string(),
        promptPrev: z.string(),
        promptNext: z.string(),
        promptFirst: z.string(),
        promptLast: z.string(),
      }),
      globalShortcut: z.object({
        enabled: z.boolean(),
        keys: z.string().max(100),
      }),
    })
    .parse({ ...state.preferences, ...input });
  if (
    !["dark", "light", "system"].includes(candidate.theme) ||
    candidate.fontSize < 11 ||
    candidate.fontSize > 24 ||
    candidate.terminalFontSize < 10 ||
    candidate.terminalFontSize > 24
  )
    throw new Error("Invalid appearance setting");
  if (
    Object.entries(candidate.shortcuts).some(([name, value]) => !shortcutAccepted(name, value)) ||
    shortcutsClash(candidate.shortcuts, process.platform)
  )
    // Shown as is in Settings, so it speaks the language Settings is in.
    throw new Error(
      translate(
        state.preferences.language,
        "Use unique shortcuts such as Mod+Shift+E.",
      ),
    );
  const problem = globalShortcutError(
    candidate.globalShortcut.keys,
    process.platform,
    (key) => translate(state.preferences.language, key),
  );
  if (problem) throw new Error(problem);
  state.preferences = candidate;
  await persist();
  return candidate;
}
