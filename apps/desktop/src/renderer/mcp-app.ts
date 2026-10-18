// MCP Apps (https://modelcontextprotocol.io/extensions/apps): an MCP server may pair a tool with a
// `ui://` page. Haarness Desktop renders that page under the tool's row, in a sandboxed frame,
// and answers the page over postMessage (JSON-RPC 2.0). This module holds the parts with no
// component in them: the payload the server attaches to the result, the page's Content Security
// Policy and document, the host's theme variables, and the bridge that decides what a page may do.
//
// What a page may do here: read the tool's input and result, size itself, read `ui://` resources
// of its own server, open http(s)/mailto links (at once after a click in the page, otherwise once
// the person agrees), and put a message in the message box (never sent by itself). Calling tools,
// updating the model's context, downloads and full screen are refused.

export const PROTOCOL_VERSION = "2026-01-26";
/** Tallest a page grows inline; a taller page scrolls inside its frame. */
export const MAX_HEIGHT = 640;
/** Height of a page that loaded but never said how tall it is. */
export const DEFAULT_HEIGHT = 360;

/** A page's own resource metadata (`_meta.ui` on its `resources/read` content). */
export type Meta = {
  csp?: {
    connectDomains?: unknown;
    resourceDomains?: unknown;
    frameDomains?: unknown;
    baseUriDomains?: unknown;
  };
  permissions?: { clipboardWrite?: unknown };
  prefersBorder?: unknown;
};

export type Result = {
  content: unknown[];
  structuredContent?: unknown;
  _meta?: unknown;
  isError?: boolean;
};

/** `mcp_app` in a tool result's metadata, as the Haarness server attaches it. */
export type Payload = {
  server: string;
  uri: string;
  tool: { name: string; title?: string; description?: string; inputSchema?: unknown };
  result?: Result;
  /** The result was too large to keep for the page. */
  tooLarge: boolean;
};

const record = (value: unknown): Record<string, unknown> | undefined =>
  value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;

/** The page a tool result asks for, read defensively from its metadata. */
export function payload(metadata: unknown): Payload | undefined {
  const value = record(record(metadata)?.mcp_app);
  if (!value) return undefined;
  const { server, uri } = value;
  if (typeof server !== "string" || !server) return undefined;
  if (typeof uri !== "string" || !uri.startsWith("ui://")) return undefined;
  const declared = record(value.tool);
  const name = typeof declared?.name === "string" ? declared.name : "";
  const result = record(value.result);
  return {
    server,
    uri,
    tool: {
      name,
      ...(typeof declared?.title === "string" && { title: declared.title }),
      ...(typeof declared?.description === "string" && { description: declared.description }),
      ...(declared && "inputSchema" in declared && { inputSchema: declared.inputSchema }),
    },
    // The stored result object itself, so an unchanged result compares equal.
    ...(result && {
      result: (Array.isArray(result.content) ? result : { ...result, content: [] }) as Result,
    }),
    tooLarge: value.result_too_large === true,
  };
}

/** The HTML and metadata a page renders, from a `resources/read` result. */
export function resource(result: unknown, uri: string): { html: string; meta: Meta } | undefined {
  const contents = record(result)?.contents;
  if (!Array.isArray(contents)) return undefined;
  const items = contents.map(record).filter((item): item is Record<string, unknown> => !!item);
  const html = (item?: Record<string, unknown>) =>
    typeof item?.mimeType === "string" && item.mimeType.startsWith("text/html");
  const item =
    items.find((entry) => entry.uri === uri && html(entry)) ??
    items.find((entry) => html(entry)) ??
    items[0];
  if (!item) return undefined;
  const text =
    typeof item.text === "string"
      ? item.text
      : typeof item.blob === "string"
        ? decode(item.blob)
        : undefined;
  if (!text) return undefined;
  return { html: text, meta: (record(record(item._meta)?.ui) ?? {}) as Meta };
}

function decode(blob: string) {
  try {
    return new TextDecoder().decode(Uint8Array.from(atob(blob), (c) => c.charCodeAt(0)));
  } catch {
    return undefined;
  }
}

/**
 * A CSP source list takes host sources only: anything else (a keyword, a quote, a semicolon)
 * could widen or end the directive it is pasted into.
 */
export function sources(list: unknown): string[] {
  if (!Array.isArray(list)) return [];
  return list.filter(
    (item): item is string =>
      typeof item === "string" &&
      /^(https?|wss?):\/\/[a-z0-9*.-]+(:\d+)?(\/[^\s;,'"]*)?$/i.test(item),
  );
}

/**
 * The page's Content Security Policy, from the domains it declares (none: no network at all).
 * `form-action` closes the one network path `default-src` leaves open, a form posting to another
 * site. Nested frames are not allowed: the host page that holds the frame refuses them anyway.
 */
export function policy(meta?: Meta) {
  const csp = meta?.csp;
  const resources = sources(csp?.resourceDomains).join(" ");
  const connect = sources(csp?.connectDomains).join(" ");
  const base = sources(csp?.baseUriDomains).join(" ");
  return [
    "default-src 'none'",
    `script-src 'self' 'unsafe-inline' ${resources}`,
    `style-src 'self' 'unsafe-inline' ${resources}`,
    `img-src 'self' data: blob: ${resources}`,
    `font-src 'self' data: ${resources}`,
    `media-src 'self' data: blob: ${resources}`,
    `connect-src ${connect || "'none'"}`,
    "frame-src 'none'",
    "object-src 'none'",
    `base-uri ${base || "'self'"}`,
    "form-action 'none'",
  ]
    .map((item) => item.trim())
    .join("; ");
}

/** The domains the host actually granted, as `hostCapabilities.sandbox.csp` reports them. */
export function granted(meta?: Meta) {
  return {
    connectDomains: sources(meta?.csp?.connectDomains),
    resourceDomains: sources(meta?.csp?.resourceDomains),
    frameDomains: [] as string[],
    baseUriDomains: sources(meta?.csp?.baseUriDomains),
  };
}

/** The host's own message keys from the page document's script (see `document`). */
export const WHEEL = "haarnessWheel";
export const LEFT = "haarnessLeft";

const attribute = (text: string) =>
  text.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");

/**
 * The page document with its policy as the first thing in it: a `<meta>` policy holds for
 * everything parsed after it and cannot be loosened by the page. It goes after a doctype, so the
 * page keeps standards mode. Next comes one host script, which holds on to the parent before the
 * page can replace it: wheel events inside a frame never reach the conversation, so it reports
 * them for the conversation to scroll; and it reports the document going away with this
 * render's `token`, so a page that leaves is closed rather than left showing something else.
 */
export function document(html: string, meta: Meta | undefined, token: string) {
  const head =
    `<meta http-equiv="Content-Security-Policy" content="${attribute(policy(meta))}">` +
    `<script>(function(p){addEventListener("wheel",function(e){p.postMessage({${WHEEL}:[e.deltaX,e.deltaY,e.deltaMode]},"*")},{passive:true,capture:true});` +
    `addEventListener("pagehide",function(){p.postMessage({${LEFT}:${JSON.stringify(token).replace(/</g, "\\u003c")}},"*")})})(parent)</script>`;
  const doctype = html.match(/^\s*<!doctype[^>]*>/i)?.[0] ?? "<!doctype html>";
  return `${doctype}${head}${html.replace(/^\s*<!doctype[^>]*>/i, "")}`;
}

/**
 * The spec's theme variables, each from a Haarness token. `color` tokens must be resolved by the
 * caller (several are `color-mix()` of other tokens, which mean nothing inside the page).
 */
export const STYLE_VARIABLES: Record<string, { token: string; color?: boolean } | { value: string }> = {
  "--color-background-primary": { token: "--surface", color: true },
  "--color-background-secondary": { token: "--raised", color: true },
  "--color-background-tertiary": { token: "--page", color: true },
  "--color-background-inverse": { token: "--text", color: true },
  "--color-background-info": { token: "--accent-soft", color: true },
  "--color-background-danger": { token: "--danger-soft", color: true },
  "--color-background-success": { token: "--green-soft", color: true },
  "--color-background-warning": { token: "--accent-soft", color: true },
  "--color-background-disabled": { token: "--hover", color: true },
  "--color-text-primary": { token: "--text", color: true },
  "--color-text-secondary": { token: "--muted", color: true },
  "--color-text-tertiary": { token: "--muted", color: true },
  "--color-text-inverse": { token: "--page", color: true },
  "--color-text-info": { token: "--accent", color: true },
  "--color-text-danger": { token: "--danger", color: true },
  "--color-text-success": { token: "--green", color: true },
  "--color-text-warning": { token: "--accent", color: true },
  "--color-text-disabled": { token: "--faint", color: true },
  "--color-border-primary": { token: "--line", color: true },
  "--color-border-secondary": { token: "--hairline", color: true },
  "--color-border-tertiary": { token: "--hairline", color: true },
  "--color-border-inverse": { token: "--text", color: true },
  "--color-border-info": { token: "--accent-edge", color: true },
  "--color-border-danger": { token: "--danger-edge", color: true },
  "--color-border-success": { token: "--green", color: true },
  "--color-border-warning": { token: "--accent-edge", color: true },
  "--color-border-disabled": { token: "--hairline", color: true },
  "--color-ring-primary": { token: "--focus", color: true },
  "--font-sans": { token: "--sans" },
  "--font-mono": { token: "--mono" },
  "--font-weight-normal": { value: "400" },
  "--font-weight-medium": { value: "500" },
  "--font-weight-semibold": { value: "600" },
  "--font-weight-bold": { value: "700" },
  "--font-text-xs-size": { token: "--ui-xs" },
  "--font-text-sm-size": { token: "--ui" },
  "--font-text-md-size": { token: "--font-size" },
  "--font-text-lg-size": { token: "--ui-lg" },
  "--font-heading-sm-size": { token: "--title-sm" },
  "--font-heading-md-size": { token: "--title" },
  "--font-heading-lg-size": { token: "--title-lg" },
  "--font-heading-xl-size": { token: "--display" },
  "--border-radius-xs": { token: "--r-xs" },
  "--border-radius-sm": { token: "--r-sm" },
  "--border-radius-md": { token: "--r-md" },
  "--border-radius-lg": { token: "--r-lg" },
  "--border-radius-xl": { token: "--r-xl" },
  "--border-radius-full": { token: "--r-pill" },
  "--border-width-regular": { value: "1px" },
  "--shadow-sm": { token: "--shadow-1" },
  "--shadow-md": { token: "--shadow-2" },
  "--shadow-lg": { token: "--shadow-2" },
};

/** The theme variables a page gets, given how to read a token and how to resolve a colour token. */
export function variables(read: (token: string) => string, color: (token: string) => string) {
  const values: Record<string, string> = {};
  for (const [name, source] of Object.entries(STYLE_VARIABLES)) {
    const value =
      "value" in source ? source.value : source.color ? color(source.token) : read(source.token);
    if (value.trim()) values[name] = value.trim();
  }
  return values;
}

/** A link a page may open: http, https and mailto only. */
export function webLink(url: unknown): string | undefined {
  if (typeof url !== "string" || url.length > 8192) return undefined;
  try {
    const parsed = new URL(url);
    return ["http:", "https:", "mailto:"].includes(parsed.protocol) ? parsed.href : undefined;
  } catch {
    return undefined;
  }
}

/** The text of a page's `ui/message`; Haarness takes only a user message made of text. */
export function messageText(params: unknown): string | undefined {
  const value = record(params);
  if (!value || (value.role !== undefined && value.role !== "user")) return undefined;
  if (!Array.isArray(value.content)) return undefined;
  const parts = value.content.map(record);
  if (!parts.length || parts.some((part) => part?.type !== "text" || typeof part.text !== "string"))
    return undefined;
  const text = parts.map((part) => part!.text as string).join("\n\n").trim();
  return text && text.length <= 32 * 1024 ? text : undefined;
}

export class RpcError extends Error {
  constructor(
    readonly code: number,
    message: string,
  ) {
    super(message);
  }
}

/** What the page's host does for it; the bridge decides what reaches it. */
export interface Host {
  initialize(params: Record<string, unknown>): unknown;
  initialized(): void;
  resize(size: { width?: number; height?: number }): void;
  /** Opens a link the bridge allowed; false when the person declined or it failed. */
  openLink(url: string): Promise<boolean>;
  /** Puts text in the message box; false when this chat cannot take it. */
  compose(text: string): boolean;
  /** Reads a `ui://` resource of the page's own server. */
  readResource(uri: string): Promise<unknown>;
  log?(params: unknown): void;
}

export const REFUSED = {
  tools: "Haarness does not let apps call tools yet. Ask in the chat instead.",
  context: "Haarness does not take context updates from apps yet.",
  download: "Haarness does not download files for apps yet.",
  read: "An app may read only ui:// resources of its own server.",
  link: "Haarness opens only http, https and mailto links from an app.",
  message: "Haarness takes only a text message from an app.",
};

type Rpc = {
  jsonrpc?: unknown;
  id?: unknown;
  method?: unknown;
  params?: unknown;
  result?: unknown;
  error?: unknown;
};

/**
 * The host side of a page's JSON-RPC: it answers the page's requests through `host`, and sends
 * the page notifications and requests of its own. Messages that are not JSON-RPC 2.0 are ignored.
 */
export function createBridge(host: Host, post: (message: Record<string, unknown>) => void) {
  let next = 0;
  const waiting = new Map<number, (value: unknown) => void>();
  async function handle(method: string, params: Record<string, unknown>) {
    switch (method) {
      case "ui/initialize":
        return host.initialize(params);
      case "ping":
        return {};
      case "ui/open-link": {
        const url = webLink(params.url);
        if (!url) throw new RpcError(-32602, REFUSED.link);
        return (await host.openLink(url)) ? {} : { isError: true };
      }
      case "ui/message": {
        const text = messageText(params);
        if (!text) throw new RpcError(-32602, REFUSED.message);
        return host.compose(text) ? {} : { isError: true };
      }
      case "resources/read": {
        const uri = params.uri;
        if (typeof uri !== "string" || !uri.startsWith("ui://")) throw new RpcError(-32602, REFUSED.read);
        return host.readResource(uri);
      }
      case "ui/request-display-mode":
        return { mode: "inline" };
      case "tools/call":
        throw new RpcError(-32601, REFUSED.tools);
      case "ui/update-model-context":
        throw new RpcError(-32601, REFUSED.context);
      case "ui/download-file":
        throw new RpcError(-32601, REFUSED.download);
      default:
        throw new RpcError(-32601, `Method not found: ${method}`);
    }
  }
  return {
    async receive(data: unknown) {
      const message = record(data) as Rpc | undefined;
      if (!message || message.jsonrpc !== "2.0") return;
      const id = message.id;
      const validId = typeof id === "string" || (typeof id === "number" && Number.isFinite(id));
      if (typeof message.method !== "string") {
        // A response to one of the host's own requests.
        if (typeof id === "number" && waiting.has(id)) {
          waiting.get(id)!(message.error ? undefined : message.result);
          waiting.delete(id);
        }
        return;
      }
      const params = record(message.params) ?? {};
      if (!validId) {
        // A notification: nothing is answered.
        if (message.method === "ui/notifications/initialized") host.initialized();
        else if (message.method === "ui/notifications/size-changed") {
          const size = (key: string) =>
            typeof params[key] === "number" && Number.isFinite(params[key]) ? (params[key] as number) : undefined;
          host.resize({ width: size("width"), height: size("height") });
        } else if (message.method === "notifications/message") host.log?.(params);
        return;
      }
      try {
        const result = await handle(message.method, params);
        post({ jsonrpc: "2.0", id, result });
      } catch (error) {
        post({
          jsonrpc: "2.0",
          id,
          error:
            error instanceof RpcError
              ? { code: error.code, message: error.message }
              : { code: -32603, message: error instanceof Error ? error.message : String(error) },
        });
      }
    },
    notify(method: string, params: Record<string, unknown> = {}) {
      post({ jsonrpc: "2.0", method, params });
    },
    /** A request to the page; resolves with its result, or undefined on an error answer. */
    request(method: string, params: Record<string, unknown> = {}) {
      const id = --next;
      post({ jsonrpc: "2.0", id, method, params });
      return new Promise<unknown>((resolve) => waiting.set(id, resolve));
    },
  };
}
