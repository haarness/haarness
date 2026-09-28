import { JsonSocket } from "./json-socket.js";
export { JsonSocket } from "./json-socket.js";
import { randomUUID } from "node:crypto";
import { open } from "node:fs/promises";
import { basename, extname } from "node:path";
import { Api } from "./api.js";
import { PtyConnection } from "./pty.js";
import type { RequestOptions } from "./transport.js";
import type { StartTurnRequest, Turn, Item } from "./types.js";
export * from "./types.js";
export * from "./capabilities.js";
export * from "./transport.js";
export * from "./pty.js";
export * from "./server.js";
export interface ChatResult {
  turn: Turn;
  items: Item[];
  text: string;
}
export class Haarness extends Api {
  /** Server-owned management forms; raw domain operations use manageResource or the dedicated helpers. */
  managementView(resource: string, body: import("./types.js").JsonValue, options: RequestOptions = {}): Promise<import("./types.js").ManagementView> {
    return this.request("POST", `/v1/manage/${this.segment(resource)}`, { body, timeoutMs: 180_000, ...options });
  }

  /** Persistent availability and definition management on this runtime. Applies when idle. */
  capabilities(input: import("./capabilities.js").CapabilityRequest = { action: "list" }, options: RequestOptions = {}): Promise<import("./types.js").JsonValue> {
    return this.request("POST", "/v1/manage/capabilities", { body: input, timeoutMs: 180_000, ...options });
  }
  /** Catalog discovery, verified installation and version lifecycle on this runtime. */
  marketplace(input: import("./capabilities.js").MarketplaceRequest, options: RequestOptions = {}): Promise<import("./types.js").JsonValue> {
    return this.request("POST", "/v1/manage/marketplace", { body: input, timeoutMs: 180_000, ...options });
  }

  /** Bind every runtime endpoint to a workspace returned by hostOpen/hostProjects. */
  workspace(id: string): Haarness {
    const headers = () => this.headers();
    return new class extends Haarness {
      protected override headers() { return headers(); }
    }({baseUrl:this.url(`/v1/host/workspaces/${this.segment(id)}`), fetch:this.fetcher});
  }

  /** Start and wait for one turn; approvals must be handled by a concurrent observer. */
  async chat(
    id: string,
    text: string,
    body: Partial<Omit<StartTurnRequest, "text">> = {},
    options: RequestOptions = {},
  ): Promise<ChatResult> {
    const turn = await this.startTurn(
      id,
      { ...body, requestId: body.requestId ?? randomUUID(), text },
      options,
    );
    const result = await this.waitTurn(
      turn.id,
      { timeout_ms: Math.min(options.timeoutMs ?? 300_000, 300_000) },
      { timeoutMs: 305_000, ...options },
    );
    const items = (await this.listItems(id, {}, options)).filter(
      (item) => item.turnId === turn.id,
    );
    const output = items
      .filter((item) => item.data.type === "assistant_message")
      .map((item) =>
        item.data.type === "assistant_message" ? item.data.text : "",
      )
      .join("");
    return { turn: result, items, text: output };
  }
  async connectBrowserHost(id: string, query: {version?: 1 | 2} = {version: 2}, options: RequestOptions = {}): Promise<JsonSocket> {
    return JsonSocket.connect(this.url(`/v1/threads/${this.segment(id)}/browser/host`, query).replace(/^http/, "ws"), {...await this.headers(), ...options.headers}, this.signal(options.signal), options.timeoutMs ?? this.timeoutMs);
  }
  async connectDraftBrowserHost(id: string, options: RequestOptions = {}): Promise<JsonSocket> {
    return JsonSocket.connect(this.url(`/v1/browser/drafts/${this.segment(id)}/host`).replace(/^http/, "ws"), {...await this.headers(), ...options.headers}, this.signal(options.signal), options.timeoutMs ?? this.timeoutMs);
  }
  async connectPty(
    id: string,
    query: { cursor?: number } = {},
    options: RequestOptions = {},
  ): Promise<PtyConnection> {
    return PtyConnection.connect(
      this.url(`/v1/ptys/${this.segment(id)}/connect`, query).replace(
        /^http/,
        "ws",
      ),
      await this.headers(),
      this.signal(options.signal),
      options.timeoutMs ?? this.timeoutMs,
    );
  }
}
const mimes: Record<string, string> = {
  ".pdf": "application/pdf",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".webp": "image/webp",
  ".gif": "image/gif",
  ".mp3": "audio/mpeg",
  ".wav": "audio/wav",
  ".mp4": "video/mp4",
  ".webm": "video/webm",
  ".m4a": "audio/mp4",
  ".ogg": "audio/ogg",
  ".flac": "audio/flac",
  ".mov": "video/quicktime",
};
/** Read local media into a remote-safe data URL. Enforces the aggregate server limit. */
export async function attachments(paths: string[]): Promise<string[]> {
  const output: string[] = [];
  let size = 0;
  for (const path of paths) {
    const mime = mimes[extname(path).toLowerCase()];
    if (!mime) throw new TypeError("Unsupported attachment extension");
    const file = await open(path, "r");
    let bytes: Buffer;
    try {
      const buffer = Buffer.alloc(20 * 1024 * 1024 - size + 1);
      let offset = 0;
      while (offset < buffer.length) {
        const chunk = await file.read(
          buffer,
          offset,
          buffer.length - offset,
          null,
        );
        if (!chunk.bytesRead) break;
        offset += chunk.bytesRead;
      }
      bytes = buffer.subarray(0, offset);
    } finally {
      await file.close();
    }
    size += bytes.length;
    if (size > 20 * 1024 * 1024)
      throw new RangeError("Attachments exceed 20 MiB");
    output.push(
      `data:${mime};name=${encodeURIComponent(basename(path))};base64,${bytes.toString("base64")}`,
    );
  }
  return output;
}

export * from "./cloud.js";

export * from "./router.js";
