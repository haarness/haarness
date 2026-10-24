import { projectMarkdown, markdownPatch, type MarkdownPatch, type MarkdownProjection } from "./markdown-project";

type Request = { key: number; revision: number; text: string; live: boolean; breaks: boolean };
type Reply = { key: number; revision: number; patch?: MarkdownPatch; error?: boolean };
type Client = {
  latest?: Request;
  running?: Request;
  timer?: ReturnType<typeof setTimeout>;
  projection?: MarkdownProjection;
  /** Drawn on this thread from now on (see `flush`). */
  local?: boolean;
  /** The revision last drawn. */
  drawn?: number;
  buffered: string[];
  rendered: string[];
  receive: (patch: MarkdownPatch, live: boolean) => void;
};
const clients = new Map<number, Client>();
let sequence = 0;
let worker: Worker | undefined;
let failed = false;
let idle: ReturnType<typeof setTimeout> | undefined;

function fallback(client: Client, request: Request) {
  const next = projectMarkdown(client.projection, request.text, request.live, request.breaks);
  // Switching from a worker requires a complete patch, since its projection
  // lives in the other realm. The DOM reconciler still retains identical nodes.
  client.receive(markdownPatch(client.projection, next), request.live);
  client.projection = next;
  client.drawn = request.revision;
}

function fail() {
  failed = true;
  worker?.terminate();
  worker = undefined;
  for (const client of clients.values()) {
    clearTimeout(client.timer);
    client.running = undefined;
    if (client.latest) fallback(client, client.latest);
  }
}

function dispatch(client: Client) {
  if (!client.latest || client.running) return;
  if (failed || client.local) return fallback(client, client.latest);
  try {
    if (!worker) {
      worker = new Worker(new URL("./markdown.worker.ts", import.meta.url), { type: "module" });
      worker.onerror = (event) => { event.preventDefault(); fail(); };
      worker.onmessageerror = fail;
      worker.onmessage = ({ data }: MessageEvent<Reply>) => {
        const current = clients.get(data.key);
        if (!current || current.running?.revision !== data.revision) return;
        clearTimeout(current.timer);
        current.running = undefined;
        if (data.error || !data.patch) return fail();
        // Deltas are ordered, but superseded text must never flash in the UI.
        current.buffered.splice(data.patch.start, Infinity, ...data.patch.html);
        current.buffered.length = data.patch.count;
        if (current.latest?.revision === data.revision) {
          let start = 0;
          while (start < current.buffered.length && current.buffered[start] === current.rendered[start]) start++;
          current.receive({ start, html: current.buffered.slice(start), count: current.buffered.length }, current.latest.live);
          current.rendered = current.buffered.slice();
          current.drawn = data.revision;
        }
        if (current.latest?.revision !== data.revision) dispatch(current);
      };
    }
    client.running = client.latest;
    client.timer = setTimeout(fail, 10_000);
    worker.postMessage(client.running);
  } catch {
    fail();
  }
}

/** `here`: drawn on this thread from the first text, as each update comes. */
export function createMarkdownClient(receive: Client["receive"], here = false) {
  clearTimeout(idle);
  const key = ++sequence;
  let revision = 0;
  let disposed = false;
  const client: Client = { receive, buffered: [], rendered: [], local: here };
  clients.set(key, client);
  return {
    update(text: string, live: boolean, breaks = true) {
      if (disposed) return;
      client.latest = { key, revision: ++revision, text, live, breaks };
      dispatch(client);
    },
    /** Draws the latest text now, on this thread, instead of when the worker
     * answers, and keeps drawing here: the worker's own state for this text
     * no longer matches what is shown. */
    flush() {
      if (disposed || !client.latest || client.drawn === client.latest.revision) return;
      clearTimeout(client.timer);
      client.running = undefined;
      client.local = true;
      fallback(client, client.latest);
    },
    dispose() {
      disposed = true;
      clearTimeout(client.timer);
      clients.delete(key);
      worker?.postMessage({ key, revision: ++revision });
      if (!clients.size) idle = setTimeout(() => { worker?.terminate(); worker = undefined; failed = false; }, 30_000);
    },
  };
}
