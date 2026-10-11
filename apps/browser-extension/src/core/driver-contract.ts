/**
 * The shared page driver's contract (`packages/browser-driver/src/types.ts`),
 * copied here until the extension depends on that package. Keep it identical
 * to the driver's: at integration this file becomes
 * `export type * from "@haarness/browser-driver/types"` and nothing else in the
 * extension changes.
 */
import type { ProtocolMapping } from "devtools-protocol/types/protocol-mapping.js";
import type * as Browser from "./browser";

export interface CdpTransport {
  send<M extends keyof ProtocolMapping.Commands>(
    method: M,
    params?: object,
    sessionID?: string,
  ): Promise<ProtocolMapping.Commands[M]["returnType"]>;
  on<E extends keyof ProtocolMapping.Events>(
    event: E,
    cb: (params: ProtocolMapping.Events[E][0], sessionID?: string) => void,
  ): () => void;
  dispose(): void;
}

export interface PageSurface {
  url(): string;
  title(): string;
  loading(): boolean;
  canGoBack(): boolean;
  canGoForward(): boolean;
  historyEntry(delta: -1 | 1): string | undefined;
  navigate(url: string, signal: AbortSignal): Promise<void>;
  back(): void;
  forward(): void;
  reload(): void;
  stop(): void;
  viewport(): Promise<{ width: number; height: number }>;
  pixelRatio(): Promise<number>;
  /** Chrome: visibility/background-tab policy. */
  readyForCapture(signal: AbortSignal): Promise<void>;
  selectAll: { keys: "Meta+A" | "Control+A" } | { commands: ["selectAll"] };
  supports(method: Browser.Method): boolean;
  /** Chrome: captures/downloads gated on the top document. */
  gateTopDocumentOnly: boolean;
  /** Chrome: securityOrigin. */
  resourceOrigins: "url" | "securityOrigin";
  cursor?: { run(code: string): Promise<void>; zoom(): number };
}

/** A file a page captured or downloaded, as `files.list` reports it. */
export interface StoredFile {
  id: Browser.FileID;
  name: string;
  mime: string;
  bytes: number;
  state: "pending" | "completed" | "failed";
}

/**
 * Where a page keeps captures and downloads. The driver's exact members are
 * still open in the contract ("add/save/get/list/transfer(id) → File | FileRef,
 * ready, dispose"); this is the extension's reading of it.
 */
export interface FileStore {
  ready: Promise<void>;
  save(name: string, mime: string, bytes: Uint8Array, resources?: string[]): Promise<Browser.FileID>;
  get(id: Browser.FileID): StoredFile | undefined;
  list(): StoredFile[];
  transfer(id: Browser.FileID): Promise<Browser.File | Browser.FileRef>;
  dispose(): Promise<void>;
}

export interface FileInput {
  upload(target: { backendNodeId: number; sessionID?: string }, files: (Browser.File | Browser.FileRef)[]): Promise<void>;
  drop(
    target: { backendNodeId: number; sessionID?: string; x: number; y: number },
    files: (Browser.File | Browser.FileRef)[],
  ): Promise<void>;
}

export interface PageOptions {
  id: Browser.TabID;
  publish(): void;
  onReset?(): void;
  refSeed?: number;
  files: FileStore;
  input: FileInput;
  translate?(key: string): string;
}

export interface Page {
  state(): Browser.Tab;
  ready: Promise<void>;
  execute(command: Browser.HostCommand, signal: AbortSignal): Promise<Browser.HostResult>;
  inspect(action: Browser.Action): Promise<Browser.Target>;
  /** Main-frame cross-document start: generation++. */
  navigationStarted(): void;
  dispose(): void;
}

export type CreatePage = (surface: PageSurface, cdp: CdpTransport, options: PageOptions) => Page;
