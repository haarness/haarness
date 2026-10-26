import { spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { Haarness } from "./index.js";
export interface ServerOptions {
  workspace: string;
  executable?: string;
  dataDir?: string;
  env?: Record<string, string | undefined>;
  /** Ignore project configuration and integrations; global settings still apply. */
  disableProjectConfig?: boolean;
  /** @deprecated Project configuration is enabled by default. False is an explicit opt-out. */
  trustProject?: boolean;
  yolo?: boolean;
  startupTimeoutMs?: number;
}
export class LocalServer {
  private stopped = false;
  private constructor(
    readonly client: Haarness,
    private readonly child: ChildProcess,
  ) {}
  /** Spawn an owned foreground process. Existing workspace servers are never replaced. */
  static async start(options: ServerOptions): Promise<LocalServer> {
    const token = randomBytes(32).toString("hex");
    const args = [
      "--workspace",
      options.workspace,
      ...(options.disableProjectConfig || options.trustProject === false
        ? ["--disable-project-config"] : []),
      ...(options.yolo ? ["--yolo"] : []),
      "serve",
      "--port",
      "0",
    ];
    const child = spawn(options.executable ?? "haarness", args, {
      env: {
        ...process.env,
        ...options.env,
        ...(options.dataDir ? { HAARNESS_DATA_DIR: options.dataDir } : {}),
        HAARNESS_SERVER_TOKEN: token,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    // What the server last said on stderr, kept (and the pipe drained) for as long as it runs: a failed start
    // reports why, such as another server already running in that folder.
    let said = "";
    child.stderr!.on("data", (chunk: Buffer) => {
      said = (said + chunk.toString()).slice(-4096);
    });
    const failure = (what: string) => {
      const reason = said.trim().split("\n").filter((line) => line.trim()).at(-1)?.trim();
      return new Error(reason ? `${what}: ${reason}` : what);
    };
    try {
      const url = await new Promise<string>((resolve, reject) => {
        let text = "";
        const timer = setTimeout(
          () => finish(failure("Haarness startup timed out")),
          options.startupTimeoutMs ?? 30_000,
        );
        const data = (chunk: Buffer) => {
          text = (text + chunk.toString()).slice(-8192);
          const match = /Haarness server listening on (http:\/\/[^\s]+)/.exec(
            text,
          );
          if (match) finish(undefined, match[1]);
        };
        const failed = (error?: Error) =>
          finish(error instanceof Error ? error : failure("Haarness failed to start"));
        function finish(error?: Error, url?: string) {
          clearTimeout(timer);
          child.stdout?.off("data", data);
          child.off("error", failed);
          child.off("close", failed);
          error ? reject(error) : resolve(url!);
        }
        child.stdout!.on("data", data);
        child.once("error", failed);
        // `close` comes after the output is read, so the reason is in `said`.
        child.once("close", () => failed());
      });
      child.stdout?.resume();
      const client = new Haarness({ baseUrl: url, token });
      const info = await client.info();
      if (info.name !== "haarness" || info.schemaVersion !== "1") {
        client.close();
        throw new Error("Incompatible Haarness server");
      }
      return new LocalServer(client, child);
    } catch (error) {
      await terminate(child);
      throw error;
    }
  }
  /** Shut down only this helper's child process and release client resources. */
  async close(): Promise<void> {
    if (this.stopped) return;
    this.stopped = true;
    try {
      if (this.child.exitCode === null)
        await this.client.shutdownServer({ timeoutMs: 3000 });
    } catch {
      /* Owned-process fallback. */
    } finally {
      this.client.close();
      await terminate(this.child);
    }
  }
}
async function terminate(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null || !child.pid)
    return;
  child.kill("SIGTERM");
  await new Promise<void>((resolve) => {
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
    }, 5000);
    child.once("exit", () => {
      clearTimeout(timer);
      resolve();
    });
    if (child.exitCode !== null) {
      clearTimeout(timer);
      resolve();
    }
  });
}
