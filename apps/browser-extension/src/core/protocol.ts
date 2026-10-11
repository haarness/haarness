/**
 * Messages between the extension, the native bridge (`haarness browser
 * native-host`) and each Haarness server the bridge relays to. The shapes are
 * the cross-lane contract (§4.1 server ↔ host, §6.4 bridge ↔ extension); the
 * extension never talks to a server except through `to-server`/`from-server`.
 */
import type { HostCommand, Outcome, State, TabID } from "./browser";

export const PROTOCOL = 1;
export const NATIVE_HOST = "ai.haarness.browser";

/** What the extension says about this browser in `hello`. */
export interface BrowserInfo {
  id: string;
  kind: "chrome";
  name: string;
  label: string | null;
  version: string;
}

export interface ControlStatus {
  paused: boolean;
  stopped: boolean;
  focusedAt: number;
}

// ---------------------------------------------------------- server ↔ host

export interface ServerAttached {
  type: "attached";
  version: number;
  lease: string;
  server: { id: string; workspace: string; pid: number };
}
export interface ServerCommand {
  type: "command";
  id: string;
  thread: string;
  title: string;
  parent: string | null;
  command: HostCommand;
}
export interface ServerCancel {
  type: "cancel";
  id: string;
}
export type FromServer = ServerAttached | ServerCommand | ServerCancel | { type: "pong" };

export type ToServer =
  | { type: "state"; thread: string; state: State; revoked?: TabID[] }
  | { type: "result"; id: string; outcome: Outcome };

// --------------------------------------------------------- bridge ↔ extension

export interface ServerEntry {
  id: string;
  workspace: string;
  name: string;
  version: string;
  state: "connecting" | "connected" | "error";
  error?: string;
}

export interface Approval {
  id: string;
  thread: string;
  patterns: string[];
  explicit: boolean;
}

export type FromBridge =
  | {
      type: "bridge";
      protocol: number;
      version: string;
      uploadsRoot: string;
      extension: { path: string; version: string } | null;
    }
  | { type: "servers"; servers: ServerEntry[] }
  | { type: "from-server"; server: string; message: FromServer }
  | { type: "activity"; server: string; active: string[]; approvals: Approval[] }
  | { type: "stopped"; server: string; thread: string; ok: boolean; error?: string }
  | { type: "thread"; server: string; thread: string; title: string; archived: boolean; parent: string | null }
  | { type: "answered"; server: string; permission: string; ok: boolean; error?: string };

export type ToBridge =
  | ({ type: "hello"; protocol: number; browser: BrowserInfo; extension: { version: string } } & ControlStatus)
  | { type: "to-server"; server: string; message: ToServer }
  | { type: "stop"; server: string; thread: string }
  | { type: "thread-info"; server: string; thread: string }
  | { type: "answer"; server: string; permission: string; decision: "always" | "once" | "reject" }
  | ({ type: "status" } & ControlStatus)
  | { type: "download"; server: string; thread: string; command: string; path: string; url: string }
  | { type: "rescan" };

// ------------------------------------------------------------- validation

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
export const isUUID = (value: unknown): value is string => typeof value === "string" && UUID.test(value);

const record = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);
const text = (value: unknown, max = 4_096): value is string => typeof value === "string" && value.length <= max;

/**
 * Reads one bridge message, or nothing when it is malformed. The bridge is
 * trusted to frame correctly but every field the extension acts on is checked,
 * since a bad message must never reach a tab.
 */
export function parseFromBridge(raw: unknown): FromBridge | undefined {
  if (!record(raw) || typeof raw.type !== "string") return;
  switch (raw.type) {
    case "bridge":
      if (typeof raw.protocol !== "number" || !text(raw.version, 64) || !text(raw.uploadsRoot)) return;
      if (raw.extension !== null && !(record(raw.extension) && text(raw.extension.path) && text(raw.extension.version, 64)))
        return;
      return raw as FromBridge;
    case "servers":
      if (!Array.isArray(raw.servers)) return;
      return {
        type: "servers",
        servers: raw.servers.filter(
          (server): server is ServerEntry =>
            record(server) &&
            text(server.id, 128) &&
            text(server.workspace) &&
            text(server.name, 256) &&
            text(server.version, 64) &&
            ["connecting", "connected", "error"].includes(server.state as string),
        ),
      };
    case "from-server": {
      if (!text(raw.server, 128)) return;
      const message = parseFromServer(raw.message);
      return message && { type: "from-server", server: raw.server, message };
    }
    case "activity":
      if (!text(raw.server, 128) || !Array.isArray(raw.active) || !Array.isArray(raw.approvals)) return;
      return {
        type: "activity",
        server: raw.server,
        active: raw.active.filter((thread): thread is string => text(thread, 128)),
        approvals: raw.approvals.filter(
          (approval): approval is Approval =>
            record(approval) &&
            text(approval.id, 128) &&
            text(approval.thread, 128) &&
            Array.isArray(approval.patterns) &&
            approval.patterns.every((pattern) => text(pattern)),
        ),
      };
    case "stopped":
    case "answered":
    case "thread":
      return text(raw.server, 128) ? (raw as FromBridge) : undefined;
  }
}

function parseFromServer(raw: unknown): FromServer | undefined {
  if (!record(raw)) return;
  switch (raw.type) {
    case "attached":
      return typeof raw.version === "number" &&
        text(raw.lease, 128) &&
        record(raw.server) &&
        text(raw.server.id, 128) &&
        text(raw.server.workspace)
        ? (raw as unknown as ServerAttached)
        : undefined;
    case "command":
      return text(raw.id, 128) &&
        text(raw.thread, 128) &&
        raw.thread.length > 0 &&
        record(raw.command) &&
        record(raw.command.action) &&
        typeof raw.command.action.type === "string"
        ? {
            type: "command",
            id: raw.id,
            thread: raw.thread,
            title: text(raw.title) ? raw.title : "",
            parent: text(raw.parent, 128) ? raw.parent : null,
            command: {
              ...(raw.command as unknown as HostCommand),
              files: Array.isArray(raw.command.files) ? (raw.command.files as HostCommand["files"]) : [],
            },
          }
        : undefined;
    case "cancel":
      return text(raw.id, 128) ? { type: "cancel", id: raw.id } : undefined;
    case "pong":
      return { type: "pong" };
  }
}
