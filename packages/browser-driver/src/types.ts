// The boundary between the shared page driver and the browser that hosts a
// tab. Desktop implements it over Electron (`apps/desktop/src/main/
// browser-chromium.ts`); Haarness for Chrome implements it over
// `chrome.debugger` (`apps/browser-extension/src/platform/chrome`). The driver
// owns everything a tool call does through the Chrome DevTools Protocol:
// snapshots and refs, input, frames, dialogs, diagnostics, captures, the
// permission targets and the generation that guards stale refs.
import type { ProtocolMapping } from "devtools-protocol/types/protocol-mapping.js";
import type { Browser } from "./rpc";

/**
 * One tab's DevTools Protocol connection.
 *
 * `sessionID` addresses a flattened child session (an out-of-process iframe
 * the page auto-attached); omit it for the tab itself. A transport delivers
 * events from the tab and from the iframe sessions announced by
 * `Target.attachedToTarget`, and drops the rest (`createCdpEvents` in `cdp.ts`
 * implements that filter for both hosts). `send` rejects with the error
 * `protocolError` (`errors.ts`) builds, so recovery hints match on every host.
 */
export interface CdpTransport {
  send<M extends keyof ProtocolMapping.Commands>(
    method: M,
    params?: object,
    sessionID?: string,
  ): Promise<ProtocolMapping.Commands[M]["returnType"]>;
  /** Subscribes to one event; returns the unsubscribe function. */
  on<E extends keyof ProtocolMapping.Events>(
    event: E,
    cb: (params: ProtocolMapping.Events[E][0], sessionID?: string) => void,
  ): () => void;
  /** Stops delivering events. The page never calls it: whoever created the transport owns it. */
  dispose(): void;
}

/** A history entry next to the current one (`historyEntry`). */
export interface HistoryEntry {
  /** Its absolute index in the tab's history; part of the back/forward permission key. */
  index: number;
  /** Its URL, or undefined past either end (the key then uses the current URL). */
  url?: string;
}

/** The overlay that shows the agent's pointer, label and edge glow (`agent-cursor.ts`). */
export interface CursorHost {
  /**
   * Runs the cursor script in an isolated world of the top document, where
   * page scripts cannot reach it. `code` is complete: it installs the overlay
   * when missing, then calls it. Resolve without running when the user turned
   * the cursor off or the tab is gone; a rejection is ignored.
   * Desktop: `executeJavaScriptInIsolatedWorld`. Chrome: `Page.createIsolatedWorld` + `Runtime.evaluate`.
   */
  run(code: string): Promise<void>;
  /** The page zoom factor: input coordinates are device-independent view pixels, the overlay draws in CSS pixels. */
  zoom(): number;
}

/**
 * What the driver needs from the browser that shows the tab. Reads are
 * synchronous because `Page.state()` is: a host whose browser answers
 * asynchronously (Chrome) keeps a cached copy current from its own events.
 */
export interface PageSurface {
  /** The top document's URL. */
  url(): string;
  title(): string;
  /** Whether the top document is loading. `wait {condition:"load"}` and back/forward/reload poll it. */
  loading(): boolean;
  canGoBack(): boolean;
  canGoForward(): boolean;
  /** The entry `delta` steps from the current one, for the permission target of back/forward. */
  historyEntry(delta: -1 | 1): HistoryEntry;
  /**
   * Loads `url` (already normalised: http(s) or about:blank) in the top frame and
   * resolves when its document has loaded; rejects with the browser's error,
   * whose `ERR_*` code `browserFailure` turns into a hint. Abort stops loading.
   */
  navigate(url: string, signal: AbortSignal): Promise<void>;
  /** Starts going back; the driver then waits until `loading()` is false. */
  back(): void | Promise<void>;
  forward(): void | Promise<void>;
  reload(): void | Promise<void>;
  stop(): void | Promise<void>;
  /** The visible viewport in CSS pixels; `scroll` wheels at its centre. */
  viewport(): Promise<{ width: number; height: number }>;
  /** Device pixels per CSS pixel for captures (zoom × display scale). */
  pixelRatio(): Promise<number>;
  /**
   * Resolves when the tab can paint a screenshot, or rejects with the
   * model-facing reason. Desktop waits for its visible view; Chrome applies its
   * background-tab policy.
   */
  readyForCapture(signal: AbortSignal): Promise<void>;
  /**
   * Called after scrolling an element into view and before measuring it.
   * Desktop forces a compositor frame, since requestAnimationFrame can stall in
   * a hidden view. Omit when the browser keeps painting.
   */
  repaint?(): Promise<void>;
  /**
   * How `fill` selects a field's text before replacing it: a key chord
   * (Desktop, per platform) or a DevTools editing command (Chrome, where ⌘A
   * from `Input.dispatchKeyEvent` does not select on macOS).
   */
  selectAll: { keys: "Meta+A" | "Control+A" } | { commands: readonly ["selectAll"] };
  /**
   * Whether this browser offers a tool. An unsupported one fails with code
   * `unsupported` before anything runs, including its permission inspection.
   */
  supports(method: Browser.Method): boolean;
  /**
   * Whether captures (screenshot) and their files are gated on the top
   * document alone (Chrome) rather than on every frame document the tab has
   * shown since its last navigation (Desktop). Ref-targeted actions always use
   * the target frame.
   */
  gateTopDocumentOnly: boolean;
  /**
   * What permission targets list: document URLs (Desktop) or the documents'
   * security origins from `Page.getFrameTree` (Chrome), so an about:blank or
   * srcdoc frame is gated as the site that owns it.
   */
  resourceOrigins: "url" | "securityOrigin";
  /**
   * Whether a navigation's permission target also lists the URL's `www.` twin
   * (`wwwTwin` in `policy.ts`), so one approval covers example.com and
   * www.example.com. Chrome only.
   */
  wwwTwin?: boolean;
  /** Draws the agent's cursor; omit for none. */
  cursor?: CursorHost;
}

/** A retained capture or download, as the driver reads it from the store. */
export interface StoredFile {
  readonly id: Browser.FileID;
  readonly name: string;
  readonly mime: string;
  readonly bytes: number;
  readonly state: "pending" | "completed" | "failed";
  /**
   * The permission resources (URLs or origins, per `resourceOrigins`) its bytes
   * came from. Exporting it needs approval for each, and `transfer` refuses
   * when the approved target no longer covers them.
   */
  readonly resources: readonly string[];
}

/** One `files.list` row; never a path or a resource. */
export interface FileListing {
  id: Browser.FileID;
  name: string;
  mime: string;
  bytes: number;
  state: "pending" | "completed" | "failed";
}

/**
 * Captures, downloads and staged uploads owned by one tab. Desktop keeps bytes
 * in a private temp folder and transfers `File`s; Chrome keeps captures in
 * memory and downloads in Chrome's folder, and transfers `FileRef`s for
 * downloads that the bridge reads. Hosts add downloads through their own API;
 * the driver only saves captures.
 */
export interface FileStore<F extends Browser.HostFile = Browser.HostFile> {
  /** Settles before the page is ready; a rejection fails `Page.ready`. */
  readonly ready: Promise<unknown>;
  /** Retains bytes; rejects over `Browser.MAX_FILE_BYTES` with a model-facing error. */
  save(
    name: string,
    mime: string,
    data: Uint8Array,
    resources: readonly string[],
  ): Promise<Browser.FileID>;
  /** A completed file; throws a model-facing error for an unknown, pending or failed one. */
  get(id: Browser.FileID): StoredFile;
  list(): readonly FileListing[];
  /**
   * The file for a tool result. With `authorized`, refuses (without exporting
   * anything) when the file's resources are no longer all in it.
   */
  transfer(id: Browser.FileID, authorized?: readonly string[]): Promise<F>;
  dispose(): Promise<void>;
}

/**
 * Hands uploaded and dropped files to Chromium. Both `DOM.setFileInputFiles`
 * and `Input.dispatchDragEvent` take local paths, and the driver sends them
 * itself, so the page's events stay trusted: a host only turns the command's
 * files into paths Chromium can read.
 */
export interface FileInput {
  /**
   * Local paths for `files`, in order. Desktop writes `File` bytes into its
   * store (they then show in `files.list`, gated on `sources`); Chrome accepts
   * only `FileRef`s the bridge staged under its uploads root. Rejects with a
   * model-facing error for anything else.
   */
  stage(
    files: readonly Browser.HostFile[],
    sources: readonly string[],
  ): Promise<readonly string[]>;
}

/** Network capture options for `network.get` (`diagnostics.ts`). */
export interface DiagnosticsOptions {
  /**
   * Whether request bodies are kept and returned. Off in Chrome, where they can
   * carry the user's own credentials: `requestBody` then reports `omitted`.
   */
  requestBodies?: boolean;
  /**
   * Header names (lower case) whose values are replaced by `<redacted>`, on top
   * of cookies and authorization, which are always redacted. Chrome passes
   * `isSensitiveHeader`.
   */
  redactHeader?(name: string): boolean;
}

/** What an adapter-handled tool receives (`PageExtension.execute`). */
export interface ExtensionContext<F extends Browser.HostFile = Browser.HostFile> {
  /** Aborts when the command is cancelled or a JavaScript dialog interrupts it. */
  signal: AbortSignal;
  /** The tab's state now, for the `tab` field of results. */
  state(): Browser.Tab;
  /** Capture resources when the command started (`Page.sources`, or the top document). */
  sources: readonly string[];
  /** Exports a retained file, checked against the approved target's resources. */
  transfer(id: Browser.FileID): Promise<F>;
  /** Wraps a JSON result, refusing one over 512 000 characters with a model-facing error. */
  result(value: unknown, files?: readonly F[]): PageResult<F>;
}

/**
 * Tools a host runs itself instead of the driver. Desktop keeps tracing, CPU
 * and heap profiling and Lighthouse, which need Node or Electron.
 */
export interface PageExtension<F extends Browser.HostFile = Browser.HostFile> {
  methods: ReadonlySet<Browser.Method>;
  /** The permission target for one of `methods`, or undefined for the driver's default rule. */
  inspect?(action: Browser.Action): Browser.Target | undefined;
  execute(action: Browser.Action, context: ExtensionContext<F>): Promise<PageResult<F>>;
}

export interface PageOptions<F extends Browser.HostFile = Browser.HostFile> {
  id: Browser.TabID;
  /** Called whenever `state()` may have changed (navigation reset, dialogs); the host sends its inventory. */
  publish(): void;
  /** Called by `navigationStarted` after the generation moved and refs, documents and diagnostics were cleared, before `publish`. */
  onReset?(): void;
  /**
   * The lowest element ref number to mint next. Refs are numbered by one
   * counter for every page in the process, so a ref from another tab or an
   * older snapshot never names a new element; a host whose process restarts
   * (Chrome's service worker) seeds it from a persisted `lastRef()`.
   */
  refSeed?: number;
  /**
   * The generation before the first navigation (default 0). Desktop passes a
   * restored tab's reserved generation minus one: its first load consumes it.
   */
  generation?: number;
  files: FileStore<F>;
  input: FileInput;
  /** The interface language's text for an English key (cursor labels). */
  translate?(key: string): string;
  /**
   * Started inside `ready`, after `files.ready` and before the DevTools
   * domains are enabled. Desktop loads the tab's first URL here; a rejection
   * fails `ready`.
   */
  start?(): Promise<void>;
  diagnostics?: DiagnosticsOptions;
  extension?: PageExtension<F>;
}

/** A tool result: JSON plus any files, bytes or paths per the host's store. */
export interface PageResult<F extends Browser.HostFile = Browser.HostFile> {
  readonly value: Browser.Result["value"];
  readonly files: readonly F[];
}

export interface Page<F extends Browser.HostFile = Browser.HostFile> {
  /** The tab as the server sees it; synchronous, from the surface and the generation. */
  state(): Browser.Tab;
  /** Resolves once the DevTools domains are enabled (and `options.start` settled). */
  readonly ready: Promise<void>;
  /**
   * Runs one command: `inspect:true` returns the permission target; otherwise
   * the target and generation must still match before the action runs. Rejects
   * with a model-facing error (`browserFailure` turns it into an outcome), or a
   * `BrowserCodeError` with a host failure code such as `unsupported`.
   */
  execute(command: Browser.HostCommand, signal: AbortSignal): Promise<PageResult<F>>;
  /** The permission target of an action: resources to gate and a key that changes when they might. */
  inspect(action: Browser.Action): Promise<Browser.Target>;
  /**
   * Tells the driver a cross-document navigation started in the top frame: the
   * generation moves on, refs, frame documents and diagnostics are dropped, and
   * the host is told to publish. Call it from the browser's navigation-start
   * event, before the new document commits.
   */
  navigationStarted(): void;
  /**
   * Sorted, de-duplicated URLs of the top document and every frame document
   * seen since the last navigation: Desktop's capture and download resources.
   */
  sources(): string[];
  /** Refuses further commands and drops its listeners and refs; the host disposes the transport, views and files. */
  dispose(): void;
}

export type CreatePage = <F extends Browser.HostFile = Browser.HostFile>(
  surface: PageSurface,
  cdp: CdpTransport,
  options: PageOptions<F>,
) => Page<F>;
