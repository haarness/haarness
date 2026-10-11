// A scripted DevTools transport and browser surface for page tests.
import type { ProtocolMapping } from "devtools-protocol/types/protocol-mapping.js";
import { createCdpEvents } from "../src/cdp";
import { Browser } from "../src/rpc";
import type {
  CdpTransport,
  FileInput,
  FileStore,
  PageSurface,
  StoredFile,
} from "../src/types";

type Handler = (params: Record<string, unknown>, sessionID?: string) => unknown;

export function fakeCdp(handlers: Record<string, Handler> = {}) {
  const events = createCdpEvents();
  const sent: { method: string; params: Record<string, unknown>; sessionID?: string }[] = [];
  const transport: CdpTransport & {
    sent: typeof sent;
    handlers: Record<string, Handler>;
    emit(name: string, params: unknown, sessionID?: string): void;
    methods(): string[];
  } = {
    sent,
    handlers,
    async send(method, params = {}, sessionID) {
      sent.push({ method, params: params as Record<string, unknown>, sessionID });
      const handler = handlers[method];
      return (handler ? await handler(params as Record<string, unknown>, sessionID) : {}) as never;
    },
    on: events.on,
    dispose: events.clear,
    emit: (name, params, sessionID) => events.receive(name, params, sessionID),
    methods: () => sent.map((entry) => entry.method),
  };
  return transport;
}

export function frameTree(
  url: string,
  securityOrigin: string,
  children: { id: string; url: string; securityOrigin: string }[] = [],
): ProtocolMapping.Commands["Page.getFrameTree"]["returnType"] {
  const frame = (id: string, url: string, securityOrigin: string, parentId?: string) =>
    ({ id, parentId, loaderId: "l", url, domainAndRegistry: "", securityOrigin, mimeType: "text/html", secureContextType: "Secure", crossOriginIsolatedContextType: "NotIsolated", gatedAPIFeatures: [] }) as never;
  return {
    frameTree: {
      frame: frame("main", url, securityOrigin),
      childFrames: children.map((child) => ({ frame: frame(child.id, child.url, child.securityOrigin, "main") })),
    },
  };
}

export function fakeSurface(overrides: Partial<PageSurface> = {}) {
  const calls: string[] = [];
  let url = "about:blank";
  const surface: PageSurface & { calls: string[]; setURL(value: string): void } = {
    calls,
    setURL: (value) => {
      url = value;
    },
    url: () => url,
    title: () => "Title",
    loading: () => false,
    canGoBack: () => true,
    canGoForward: () => false,
    historyEntry: (delta) => ({ index: 3 + delta, url: delta < 0 ? "https://back.test/" : undefined }),
    async navigate(next) {
      calls.push("navigate " + next);
      url = next;
    },
    back: () => void calls.push("back"),
    forward: () => void calls.push("forward"),
    reload: () => void calls.push("reload"),
    stop: () => void calls.push("stop"),
    viewport: async () => ({ width: 800, height: 600 }),
    pixelRatio: async () => 2,
    readyForCapture: async () => void calls.push("readyForCapture"),
    selectAll: { keys: "Control+A" },
    supports: () => true,
    gateTopDocumentOnly: false,
    resourceOrigins: "url",
    ...overrides,
  };
  return surface;
}

export function memoryFiles() {
  const files = new Map<Browser.FileID, StoredFile & { data: Uint8Array }>();
  let next = 0;
  const store: FileStore<Browser.File> & { files: typeof files } = {
    files,
    ready: Promise.resolve(),
    async save(name, mime, data, resources) {
      const id = Browser.FileID.make(`file_00000000-0000-4000-8000-${String(++next).padStart(12, "0")}`);
      files.set(id, { id, name, mime, bytes: data.byteLength, state: "completed", resources: [...new Set(resources)].sort(), data });
      return id;
    },
    get(id) {
      const file = files.get(id);
      if (!file) throw new Error("File ID is not retained in this tab.");
      return file;
    },
    list: () => Array.from(files.values(), ({ id, name, mime, bytes, state }) => ({ id, name, mime, bytes, state })),
    async transfer(id, authorized) {
      const file = store.get(id) as StoredFile & { data: Uint8Array };
      if (authorized && file.resources.some((url) => !authorized.includes(url)))
        throw new Error("Capture source changed before export.");
      return { id, name: file.name, mime: file.mime, data: file.data };
    },
    dispose: async () => files.clear(),
  };
  return store;
}

export function pathInput(): FileInput & { staged: unknown[] } {
  const staged: unknown[] = [];
  return {
    staged,
    async stage(files, sources) {
      staged.push({ files, sources });
      return files.map((file) => ("path" in file ? file.path : `/tmp/${file.name}`));
    },
  };
}

export const tabID = Browser.TabID.make("tab_00000000-0000-4000-8000-000000000001");
