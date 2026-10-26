/**
 * The test's side of the fake native host: it plays the Haarness bridge (and
 * the servers behind it). Every host process Chrome starts connects here, and
 * the newest connection is the live one, as with a real bridge restart.
 */
import { createServer, type Server, type Socket } from "node:net";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { workspaceVersion } from "../../manifest.config";

export interface Received {
  connection: number;
  message: Record<string, unknown> & { type: string };
}

export const SERVER = "server-1";
export const WORKSPACE = "/work/acme";
export const THREAD = "0192a5f0-7c1e-7d3a-9b1e-4f2a9c3d5e61";
export const OTHER_THREAD = "0192a5f0-7c1e-7d3a-9b1e-4f2a9c3d5e62";
export const UPLOADS = "/tmp/haarness-e2e-uploads";
/** The release under test: the built extension reports it, and a bridge and servers of the same release say it. */
export const VERSION = workspaceVersion(resolve(dirname(fileURLToPath(import.meta.url)), "../../../../Cargo.toml"));

export class FakeBridge {
  readonly received: Received[] = [];
  private readonly sockets: Socket[] = [];
  private waiters: { match(entry: Received): boolean; resolve(entry: Received): void }[] = [];
  private server!: Server;
  port = 0;
  private sequence = 0;

  async listen() {
    this.server = createServer((socket) => {
      const connection = this.sockets.push(socket);
      let buffer = "";
      socket.setEncoding("utf8");
      socket.on("data", (chunk: string) => {
        buffer += chunk;
        let index;
        while ((index = buffer.indexOf("\n")) >= 0) {
          const line = buffer.slice(0, index);
          buffer = buffer.slice(index + 1);
          if (!line.trim()) continue;
          const entry = { connection, message: JSON.parse(line) } as Received;
          this.received.push(entry);
          this.waiters = this.waiters.filter((waiter) => !(waiter.match(entry) && (waiter.resolve(entry), true)));
        }
      });
      socket.on("error", () => {});
    });
    await new Promise<void>((resolve) => this.server.listen(0, "127.0.0.1", resolve));
    const address = this.server.address();
    this.port = typeof address === "object" && address ? address.port : 0;
    return this;
  }

  get connections() {
    return this.sockets.length;
  }

  /** Resolves with the first message (already received or still to come) that matches. */
  wait(match: (message: Received["message"], entry: Received) => boolean, timeout = 15_000, from = 0): Promise<Received> {
    const test = (entry: Received) => match(entry.message, entry);
    const seen = this.received.slice(from).find(test);
    if (seen) return Promise.resolve(seen);
    return new Promise((resolve, reject) => {
      const waiter = { match: test, resolve };
      this.waiters.push(waiter);
      setTimeout(() => {
        this.waiters = this.waiters.filter((other) => other !== waiter);
        reject(new Error(`Timed out waiting for a message from the extension. Received: ${JSON.stringify(this.received.slice(-8).map((entry) => entry.message))}`));
      }, timeout);
    });
  }

  send(message: unknown) {
    const socket = this.sockets.at(-1);
    if (!socket) throw new Error("No host connected");
    socket.write(JSON.stringify(message) + "\n");
  }

  /** What a real bridge says first, then the projects it found. */
  start(options: { extension?: { path: string; version: string } | null } = {}) {
    this.send({
      type: "bridge",
      protocol: 1,
      version: VERSION,
      uploadsRoot: UPLOADS,
      extension: options.extension ?? null,
    });
    this.send({
      type: "servers",
      servers: [{ id: SERVER, workspace: WORKSPACE, name: "acme", version: VERSION, state: "connected" }],
    });
  }

  attach(instance = "instance-1") {
    this.send({
      type: "from-server",
      server: SERVER,
      message: { type: "attached", version: 1, lease: "lease-1", server: { id: instance, workspace: WORKSPACE, pid: 4242 } },
    });
  }

  /** Sends a command as a server would and resolves with its result (and every message before it). */
  async command(command: Record<string, unknown>, options: { thread?: string; title?: string; parent?: string | null } = {}) {
    const id = `command-${++this.sequence}`;
    const from = this.received.length;
    this.send({
      type: "from-server",
      server: SERVER,
      message: {
        type: "command",
        id,
        thread: options.thread ?? THREAD,
        title: options.title ?? "Research trip",
        parent: options.parent ?? null,
        command: { files: [], ...command },
      },
    });
    const result = await this.wait((message) => isToServer(message, "result") && inner(message).id === id, 30_000, from);
    return { id, result: inner(result.message) as { id: string; outcome: Outcome }, before: this.received.slice(from, this.received.indexOf(result)) };
  }

  async close() {
    for (const socket of this.sockets) socket.destroy();
    await new Promise((resolve) => this.server.close(resolve));
  }
}

export type Outcome =
  | { type: "success"; result: { value: any; files: unknown[] } }
  | { type: "failure"; code: string; message: string };

export function isToServer(message: Received["message"], type: string) {
  return message.type === "to-server" && (message.message as { type?: string })?.type === type;
}

export function inner(message: Received["message"]) {
  return message.message as Record<string, any>;
}

/** The policy a server sends with each Chrome command: every site asks first. */
export const askPolicy = {
  enabled: true,
  rememberApprovals: true,
  defaults: { browsing: "ask", downloads: "ask", uploads: "ask" },
  sites: [] as { origin: string; browsing: string; downloads: string; uploads: string }[],
};
