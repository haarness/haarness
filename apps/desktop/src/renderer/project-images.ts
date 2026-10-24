/*
 * Images a reply names by a path in its project: `![chart](out/chart.png)`,
 * `./out/chart.png`, an absolute path under the project or a local `file:`
 * URL. The page never loads a path itself: Markdown shows a placeholder in
 * the image's place, and the app reads the file through the server's file
 * API, which reads only inside the project (a link is followed to where it
 * leads and refused when that is outside) and never Git metadata. The image
 * then shows as a data URL. Only raster images whose bytes say so are shown:
 * an SVG is a document with scripts and links of its own. A path outside the
 * project is never asked for, and its placeholder says so.
 *
 * What was read is remembered in memory, so a reply drawn again (scrolled
 * back into view, or the live reply once it is saved) shows its image at
 * once, with no placeholder in between, and a reply asks for each of its
 * images once. A remembered image keeps showing while it is read again behind
 * it, and every reply showing it takes the new one when it differs. While a
 * reply shows it, it is read again for them:
 * - at once after the chat being viewed (itself, not its subagents) ends a
 *   turn, or after a write tool or the file API wrote that very file;
 * - when `PROJECT_IMAGE_LIMITS.recheck` has passed since it was read, after
 *   another change of the project (any other turn ended, a tool of the viewed
 *   chat wrote files, the window of a project on this computer came back to
 *   the front), or after a read that got no answer;
 * so an image on screen is read at most once per change, and at most once
 * per recheck time for the changes that wait. Pure apart from the DOM it is
 * handed, so it is tested without a window.
 */
import { linkTarget, parsedAddress } from "./link-target";
import { madeHere, markMade } from "./remote-images";
import type { FileContent } from "../shared";

/**
 * The reads that run at once across every reply, how long one may run (in
 * milliseconds) before it ends as unavailable, the paths one reply reads, the
 * image data remembered across all projects (the oldest goes first), and how
 * long a remembered image is shown before a change of the project has it read
 * again. The server reads files of at most 10 MiB.
 */
export const PROJECT_IMAGE_LIMITS = {
  parallel: 3,
  wait: 30_000,
  paths: 20,
  bytes: 64 * 1024 * 1024,
  recheck: 20_000,
  /** An image with more pixels is not decoded ("Too large to show"): 50 megapixels. */
  pixels: 50_000_000,
  /** The least time between two reads of an image on screen, however often its file is written. */
  again: 1_000,
};

// Raster types the window draws. An SVG is a document, so it is not read.
const RASTER: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  avif: "image/avif",
  bmp: "image/bmp",
  ico: "image/x-icon",
};

/** Why a reply's image shows no picture: outside the project, too large, or not readable as an image. */
export type ImageNote = "outside" | "large" | "failed";

/** A path in the project (relative, `/`-separated), or why a path is not read. */
export type ProjectImagePath = { path: string } | { refused: ImageNote };

/**
 * A Windows path as Desktop reads it: without the `\\?\` prefix a
 * canonical path carries (`\\?\C:\proj`), which would otherwise read as a
 * network path. One on a share (`\\?\UNC\host\share`) keeps it, and so
 * stays one.
 */
export const windowsPath = (path: string) => path.replace(/^[\\/]{2}\?[\\/](?!UNC[\\/])/i, "");

/**
 * The folders a project is known by: the one Desktop opened, and the one the
 * server reports, where links are resolved (macOS's `/private/tmp` for
 * `/tmp`, a synced folder's real place); a path under either is in it.
 */
export function projectRoots(connection: { workspace: string; info?: { workspace?: string | null } }) {
  return [...new Set([connection.workspace, connection.info?.workspace ?? ""].map(windowsPath).filter(Boolean))];
}

/**
 * What an image source names, decided before anything is read. Undefined
 * when it is not a path at all (a remote address, `data:`, `blob:`, a
 * fragment, a network path), which stays as the sanitizer left it. A path
 * that leaves the project (every one of `roots`), or a local `file:` URL
 * there, is refused as outside; one that is not a raster image, or is
 * spelled with a control character, as unreadable.
 */
export function projectImagePath(
  src: string | null | undefined,
  roots: string | readonly string[],
  /** A chat's own folder (relative to the project), which relative paths
   * start from and no path may leave. */
  within?: string,
): ProjectImagePath | undefined {
  const raw = windowsPath(parsedAddress(src ?? ""));
  if (!raw || raw.startsWith("#") || /^[\\/]{2}/.test(raw)) return undefined;
  const scheme = /^([a-z][a-z\d+.-]*):/i.exec(raw)?.[1]?.toLowerCase();
  const drive = scheme?.length === 1 && /^[a-z]:[\\/]/i.test(raw);
  if (scheme && !drive && scheme !== "file") return undefined;
  const target = (typeof roots === "string" ? [roots] : roots)
    .filter(Boolean)
    .map((root) => linkTarget(raw, { root: windowsPath(root), within }))
    .find((found) => found.kind === "file");
  if (target?.kind !== "file") return { refused: "outside" };
  if (/[\x00-\x1f\x7f]/.test(target.path)) return { refused: "failed" };
  if (!imageType(target.path)) return { refused: "failed" };
  return { path: target.path };
}

/** The type a path's extension names, for the raster types only. */
export function imageType(path: string): string | undefined {
  return RASTER[/\.([^./]+)$/.exec(path)?.[1]?.toLowerCase() ?? ""];
}

/** The raster type these bytes start as, whatever the file is named. */
export function sniffImage(bytes: Uint8Array): string | undefined {
  const has = (at: number, sign: number[] | string) =>
    (typeof sign === "string" ? Array.from(sign, (char) => char.charCodeAt(0)) : sign).every(
      (byte, index) => bytes[at + index] === byte,
    );
  if (has(0, [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) return "image/png";
  if (has(0, [0xff, 0xd8, 0xff])) return "image/jpeg";
  if (has(0, "GIF87a") || has(0, "GIF89a")) return "image/gif";
  if (has(0, "RIFF") && has(8, "WEBP")) return "image/webp";
  if (has(0, "BM")) return "image/bmp";
  if (has(0, [0, 0, 1, 0])) return "image/x-icon";
  if (has(4, "ftyp")) {
    // An ISO media file: its major brand and the first compatible ones.
    for (let at = 8; at + 4 <= Math.min(bytes.length, 64); at += 4) {
      if (at === 12) continue;
      const brand = String.fromCharCode(...bytes.subarray(at, at + 4));
      if (brand === "avif" || brand === "avis") return "image/avif";
    }
  }
  return undefined;
}

/**
 * Bytes `offset` to `offset + length` of base64 text, decoding only the part
 * that holds them (four characters are three bytes), so a header is read
 * without decoding a whole image.
 */
function slice(base64: string) {
  return (offset: number, length: number) => {
    const from = Math.floor(offset / 3) * 4;
    const to = Math.min(base64.length, Math.ceil((offset + length) / 3) * 4);
    if (from >= to) return new Uint8Array();
    try {
      const skip = offset - (from / 4) * 3;
      return Uint8Array.from(atob(base64.slice(from, to)).slice(skip, skip + length), (char) => char.charCodeAt(0));
    } catch {
      return new Uint8Array();
    }
  };
}

/** The bytes base64 text decodes to, from its length alone. */
const decodedSize = (base64: string) =>
  Math.floor((base64.length * 3) / 4) - (base64.endsWith("==") ? 2 : base64.endsWith("=") ? 1 : 0);

const u16be = (bytes: Uint8Array, at: number) => (bytes[at] << 8) | bytes[at + 1];
const u16le = (bytes: Uint8Array, at: number) => bytes[at] | (bytes[at + 1] << 8);
const u32be = (bytes: Uint8Array, at: number) => (u16be(bytes, at) * 65536 + u16be(bytes, at + 2)) >>> 0;
const u32le = (bytes: Uint8Array, at: number) => (u16le(bytes, at) + u16le(bytes, at + 2) * 65536) >>> 0;
const text = (bytes: Uint8Array) => String.fromCharCode(...bytes);

/** A JPEG's size, from its first start-of-frame segment. */
function jpegSize(read: (offset: number, length: number) => Uint8Array, total: number) {
  let offset = 2;
  for (let segments = 0; segments < 1000 && offset + 4 <= total; segments++) {
    const head = read(offset, 4);
    if (head[0] !== 0xff) return undefined;
    const marker = head[1];
    // Fill bytes, and markers that stand alone.
    if (marker === 0xff) {
      offset += 1;
      continue;
    }
    if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd8)) {
      offset += 2;
      continue;
    }
    if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
      const frame = read(offset + 5, 4);
      return frame.length === 4 ? { width: u16be(frame, 2), height: u16be(frame, 0) } : undefined;
    }
    offset += 2 + u16be(head, 2);
  }
  return undefined;
}

/**
 * An image's type, from the way its bytes start, and its size in pixels from
 * its header when the type says where (PNG, GIF, BMP, WebP, JPEG, ICO, and an
 * AVIF whose `ispe` property is near its start), without decoding it.
 */
export function imageHeader(base64: string): { type: string; width?: number; height?: number } | undefined {
  const read = slice(base64);
  const type = sniffImage(read(0, 48));
  if (!type) return undefined;
  const size = (() => {
    if (type === "image/png") {
      const bytes = read(16, 8);
      return { width: u32be(bytes, 0), height: u32be(bytes, 4) };
    }
    if (type === "image/gif") {
      const bytes = read(6, 4);
      return { width: u16le(bytes, 0), height: u16le(bytes, 2) };
    }
    if (type === "image/bmp") {
      const bytes = read(14, 12);
      if (u32le(bytes, 0) === 12) return { width: u16le(bytes, 4), height: u16le(bytes, 6) };
      return { width: Math.abs(u32le(bytes, 4) | 0), height: Math.abs(u32le(bytes, 8) | 0) };
    }
    if (type === "image/x-icon") {
      const bytes = read(6, 2);
      return { width: bytes[0] || 256, height: bytes[1] || 256 };
    }
    if (type === "image/webp") {
      const chunk = text(read(12, 4));
      if (chunk === "VP8 ") {
        const bytes = read(26, 4);
        return { width: u16le(bytes, 0) & 0x3fff, height: u16le(bytes, 2) & 0x3fff };
      }
      if (chunk === "VP8L") {
        const [b0, b1, b2, b3] = read(21, 4);
        return { width: 1 + (((b1 & 0x3f) << 8) | b0), height: 1 + (((b3 & 0x0f) << 10) | (b2 << 2) | (b1 >> 6)) };
      }
      if (chunk === "VP8X") {
        const bytes = read(24, 6);
        return { width: 1 + (bytes[0] | (bytes[1] << 8) | (bytes[2] << 16)), height: 1 + (bytes[3] | (bytes[4] << 8) | (bytes[5] << 16)) };
      }
      return undefined;
    }
    if (type === "image/jpeg") return jpegSize(read, decodedSize(base64));
    // AVIF: the image spatial extents property, when it is near the start.
    const start = read(0, Math.min(decodedSize(base64), 64 * 1024));
    for (let at = 0; at + 16 <= start.length; at++)
      if (start[at] === 0x69 && text(start.subarray(at, at + 4)) === "ispe")
        return { width: u32be(start, at + 8), height: u32be(start, at + 12) };
    return undefined;
  })();
  return size && size.width > 0 && size.height > 0 ? { type, ...size } : { type };
}

/**
 * A file the server returned, as an image to show: a data URL of the type
 * its bytes start as, or undefined when it is text or no image the window
 * draws.
 */
export function fileImage(file: { content?: string; encoding?: string } | undefined): string | undefined {
  if (!file?.content || file.encoding !== "base64") return undefined;
  const type = imageHeader(file.content)?.type;
  return type ? `data:${type};base64,${file.content}` : undefined;
}

/** Whether an image's header says it has more pixels than the window decodes for a reply. */
export function tooManyPixels(base64: string) {
  const header = imageHeader(base64);
  return !!header?.width && !!header.height && header.width * header.height > PROJECT_IMAGE_LIMITS.pixels;
}

/** Image types a read tool's own result shows as it carries them. */
const CARRIED = /^data:(image\/(?:png|jpeg|gif|webp))((?:;[^,;]*)*);base64,/i;

/**
 * The image a finished `read` returned, to preview in its row: the image the
 * result carries (a data URL the server made smaller), or the project file it
 * read, which is read again like a reply's image and shown only while it has
 * the size the result recorded (`size`), so a file changed since is not
 * passed off as what the agent read. An image outside the project that the
 * result does not carry is not shown.
 */
export function readToolImage(
  name: string,
  result: { is_error: boolean; metadata?: unknown } | undefined,
  root: string | readonly string[],
): { data: string } | { path: string; size?: number } | undefined {
  if (name !== "read" || !result || result.is_error) return undefined;
  const metadata = result.metadata as { attachments?: unknown; mime?: unknown; size?: unknown } | undefined;
  const source = Array.isArray(metadata?.attachments) ? metadata.attachments[0] : undefined;
  if (typeof source !== "string" || typeof metadata?.mime !== "string" || !metadata.mime.startsWith("image/"))
    return undefined;
  const carried = CARRIED.exec(source);
  if (carried) {
    const payload = source.slice(carried[0].length);
    const data = fileImage({ content: payload, encoding: "base64" });
    return data && !tooManyPixels(payload) ? { data } : undefined;
  }
  if (/^data:/i.test(source)) return undefined;
  const found = projectImagePath(source, root);
  if (!found || !("path" in found)) return undefined;
  return typeof metadata.size === "number" ? { path: found.path, size: metadata.size } : found;
}

/** Reads one project file through the server's file API. */
export type ImageReader = (
  path: string,
) => Promise<{ content?: string; encoding?: string; size?: number } | undefined>;

/** What a reply shows for a path: the image and its file's size in bytes, or why there is none. */
export type ShownImage = { data: string; size?: number; note?: undefined } | { data?: undefined; note: ImageNote };

/**
 * A remembered answer: the project's changes it was read after (those that
 * wait for the recheck time, and those that do not), when, and the writes of
 * the file then. `transient` marks one the read could not settle (a timeout,
 * a dropped connection): it is read again as after a change.
 */
type Marks = { epoch: number; urgent: number; at: number; touch: number; transient?: boolean };
type Entry = ShownImage & Marks;

const remembered = new Map<string, Entry>();
let rememberedBytes = 0;
/**
 * When each image was last read and after which changes, kept when its
 * picture is let go for room: an image on screen whose picture was let go is
 * read again for a change on the same terms as one remembered.
 */
const reads = new Map<string, Marks>();

function marked(entry: string, value: Marks) {
  reads.delete(entry);
  reads.set(entry, { epoch: value.epoch, urgent: value.urgent, at: value.at, touch: value.touch, transient: value.transient });
  for (const oldest of reads.keys()) {
    if (reads.size <= 5000) break;
    reads.delete(oldest);
  }
}
/** Each project's changes since this window started: those read again after the recheck time, and those at once. */
const epochs = new Map<string, number>();
const urgents = new Map<string, number>();
/** The file API's writes of each file. */
const touches = new Map<string, number>();
/** Projects on this computer, which the window coming to the front marks as changed. */
const local = new Set<string>();
const key = (scope: string, path: string) => `${scope}\n${path}`;
const epoch = (scope: string) => epochs.get(scope) ?? 0;
const urgent = (scope: string) => urgents.get(scope) ?? 0;

function remember(entry: string, value: Entry) {
  marked(entry, value);
  const limit = PROJECT_IMAGE_LIMITS.bytes;
  const previous = remembered.get(entry);
  if (previous) {
    rememberedBytes -= previous.data?.length ?? 0;
    remembered.delete(entry);
  }
  if ((value.data?.length ?? 0) > limit) return;
  remembered.set(entry, value);
  rememberedBytes += value.data?.length ?? 0;
  // Notes weigh nothing, so they are bounded by count.
  for (const [oldest, old] of remembered) {
    if (rememberedBytes <= limit && remembered.size <= 2000) break;
    remembered.delete(oldest);
    rememberedBytes -= old.data?.length ?? 0;
  }
}

/** What this project last showed for the path, even when it is read again behind it. */
export function shownProjectImage(scope: string, path: string): ShownImage | undefined {
  const entry = remembered.get(key(scope, path));
  if (!entry) return undefined;
  return entry.data !== undefined ? { data: entry.data, size: entry.size } : { note: entry.note! };
}

/**
 * The project may have changed on disk: what it showed keeps showing, and the
 * images replies show are read again for them once
 * `PROJECT_IMAGE_LIMITS.recheck` has passed since they were read. With
 * `now`, as when the chat being viewed ends a turn, at once.
 */
export function projectChanged(scope: string, now = false) {
  if (now) urgents.set(scope, urgent(scope) + 1);
  else epochs.set(scope, epoch(scope) + 1);
  for (const entry of [...askers.keys()]) if (entry.startsWith(`${scope}\n`)) askAgain(entry, now);
}

/** The file was written: replies showing it, and the next one to draw it, read it again at once. */
export function projectFileWritten(scope: string, path: string) {
  const entry = key(scope, path.replace(/\\/g, "/").replace(/^\.\//, ""));
  touches.set(entry, (touches.get(entry) ?? 0) + 1);
  askAgain(entry, true);
}

/**
 * The window came back to the front, where the person may have changed files
 * in another app: projects on this computer may have changed. A remote host's
 * files did not change by that, so they are not read again for it.
 */
export function windowFocused() {
  for (const scope of local) projectChanged(scope);
}

/** A project on this computer, whose files the window coming to the front may have changed. */
export function localProject(scope: string) {
  local.add(scope);
}

/**
 * Whether `thread` is the chat `selected` itself ("self"), or it or the tree
 * `root` heads is in the tree that chat belongs to, its subagents included
 * ("tree"); undefined otherwise.
 */
export function inViewedTree(
  selected: string | null | undefined,
  threads: readonly { id: string; parentId?: string | null }[],
  thread?: string | null,
  root?: string | null,
): "self" | "tree" | undefined {
  if (!selected) return undefined;
  if (thread === selected) return "self";
  let top = selected;
  for (let guard = 0; guard < 64; guard++) {
    const parent = threads.find((item) => item.id === top)?.parentId;
    if (!parent) break;
    top = parent;
  }
  return [thread, root].some((id) => !!id && (id === selected || id === top)) ? "tree" : undefined;
}

/** What a server event means for a project's images (see `projectEvent`). */
export type ProjectChange =
  /** The file API wrote this file. */
  | { written: string }
  /** A write or edit tool call of `thread` names this image; it is written once its result says so. */
  | { writing: string; call: string; thread: string }
  /** A tool call of `thread` finished; if it was writing an image and did not fail, that file is written. */
  | { finished: string; thread: string; failed?: boolean }
  /** Files may have changed: read again after the recheck time, or `now`. */
  | { changed: true; now?: boolean };

/**
 * What a server event means for a project's images. The file API wrote a
 * file. A write or edit tool call names an image of the project, which is
 * written only once its result arrives (it may wait for approval, or fail).
 * The chat being viewed ended a turn: its images are read again at once.
 * Another turn ended (another chat's, or a subagent's of the viewed chat), or
 * a tool of the viewed chat or its subagents that writes (as the Files panel
 * reads them) finished: files may have changed, read again after the recheck
 * time. Another chat's tools, however busy, change nothing
 * on their own; its turn's end, once per turn, does. `viewed` says whether an
 * event's chat (or its tree) is the one being viewed, and `roots` are the
 * project's folders, which a tool's absolute path is read against.
 */
export function projectEvent(
  event: { type?: string; payload?: unknown; threadId?: string | null; rootThreadId?: string | null },
  viewed: (thread?: string | null, root?: string | null) => ReturnType<typeof inViewedTree>,
  roots: string | readonly string[] = [],
): ProjectChange | undefined {
  if (event.type === "files.updated") {
    const path = (event.payload as { path?: unknown } | undefined)?.path;
    return typeof path === "string" && path ? { written: path } : undefined;
  }
  const mine = () => viewed(event.threadId, event.rootThreadId);
  const thread = event.threadId ?? "";
  if (event.type === "turn.finished") return { changed: true, now: mine() === "self" };
  if (event.type !== "item.created") return undefined;
  const data = (
    event.payload as
      | {
          data?: {
            type?: string;
            name?: string;
            call_id?: string;
            is_error?: boolean;
            arguments?: Record<string, unknown>;
          };
        }
      | undefined
  )?.data;
  if (data?.type === "tool_call" && /(?:^|\.)(?:write|edit|multiedit)$/i.test(data.name ?? "") && data.call_id) {
    const named = [data.arguments?.path, data.arguments?.filePath, data.arguments?.file_path].find(
      (value): value is string => typeof value === "string",
    );
    const found = named === undefined ? undefined : projectImagePath(named, roots);
    return found && "path" in found ? { writing: found.path, call: data.call_id, thread } : undefined;
  }
  if (data?.type !== "tool_result") return undefined;
  if (data.is_error) return data.call_id ? { finished: data.call_id, thread, failed: true } : undefined;
  const writes = /write|edit|patch|apply|bash|shell|move|delete/.test(data.name ?? "");
  if (writes && mine()) return { changed: true };
  return data.call_id ? { finished: data.call_id, thread } : undefined;
}

/** Image files each project's write tool calls are writing, by chat and call, until their results arrive. */
const writing = new Map<string, string>();
const call = (scope: string, thread: string, id: string) => `${scope}\n${thread}\n${id}`;

/** Applies a server event of project `scope` to what its images show (see `projectEvent`). */
export function applyProjectEvent(
  scope: string,
  event: Parameters<typeof projectEvent>[0],
  viewed: Parameters<typeof projectEvent>[1],
  roots: Parameters<typeof projectEvent>[2] = [],
) {
  const change = projectEvent(event, viewed, roots);
  if (!change) return;
  if ("written" in change) return projectFileWritten(scope, change.written);
  if ("writing" in change) return void writing.set(call(scope, change.thread, change.call), change.writing);
  const id =
    "finished" in change
      ? change.finished
      : (event.payload as { data?: { call_id?: string } } | undefined)?.data?.call_id;
  const done = id === undefined ? undefined : call(scope, event.threadId ?? "", id);
  const path = done === undefined ? undefined : writing.get(done);
  if (done !== undefined && path !== undefined) {
    writing.delete(done);
    if (!("failed" in change && change.failed)) projectFileWritten(scope, path);
  }
  if ("changed" in change) projectChanged(scope, change.now);
}

type Settled = (scope: string, path: string, shown: ShownImage) => void;
const listeners = new Set<Settled>();

/**
 * Calls `listener` whenever a read changes what a path shows. Every mounted
 * reply listens, so each swaps in its own images: another reply with the same
 * image, or one drawn anew while the read ran, is not left waiting.
 */
export function onProjectImageSettled(listener: Settled): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function settle(scope: string, path: string, shown: ShownImage) {
  for (const listener of [...listeners]) {
    try {
      listener(scope, path, shown);
    } catch {
      /* One reply's trouble never keeps the others waiting. */
    }
  }
}

type Job = { waiters: Set<AbortSignal>; start: () => void };
const pending = new Map<string, Job>();
const waiting: Job[] = [];
let active = 0;

/** Starts the reads that wait, a few at a time; one whose replies have all gone is never started. */
function pump() {
  while (active < PROJECT_IMAGE_LIMITS.parallel && waiting.length) {
    const job = waiting.shift()!;
    job.start();
  }
}

/** Whether the server refused the file for its size. */
const tooLarge = (error: unknown) => /\bexceeds\b/i.test(error instanceof Error ? error.message : String(error));

/**
 * Whether the server answered that the file is not there or not one it reads
 * (gone, out of the project, Git metadata, not a regular file), which is an
 * answer, unlike a timeout or a dropped connection.
 */
const refused = (error: unknown) =>
  /no longer exists|escapes the workspace|must be relative|Git metadata|Only regular files/i.test(
    error instanceof Error ? error.message : String(error),
  );

/** A read that answered nothing: the remembered image stays, and is read again on the next occasion. */
const UNSETTLED = Symbol("unsettled");

type Decode = (data: string) => Promise<boolean | "large">;
type Asker = { read: ImageReader; decode: Decode };
/** The replies showing each image, by the signal that ends when they unmount, and how they read it. */
const askers = new Map<string, Map<AbortSignal, Asker>>();
/** The read each image on screen waits for until the recheck time ends. */
const trailing = new Map<string, ReturnType<typeof setTimeout>>();

function enlist(entry: string, signal: AbortSignal, asker: Asker) {
  let shown = askers.get(entry);
  if (!shown) askers.set(entry, (shown = new Map()));
  if (shown.has(signal)) return;
  shown.set(signal, asker);
  signal.addEventListener(
    "abort",
    () => {
      shown!.delete(signal);
      if (!shown!.size && askers.get(entry) === shown) {
        askers.delete(entry);
        clearTimeout(trailing.get(entry));
        trailing.delete(entry);
      }
    },
    { once: true },
  );
}

/**
 * Asks again for the image at `entry` for the replies still showing it, whose
 * answer reaches them all: read now, or when the recheck time ends, as `due`
 * says. Each of them asks, so the read stands while any of them is on screen;
 * a read running or waiting, and one waiting for the recheck time, is shared.
 * With `now`, a read waiting for the recheck time is dropped first.
 */
function askAgain(entry: string, now = false) {
  if (now) {
    clearTimeout(trailing.get(entry));
    trailing.delete(entry);
  }
  const at = entry.indexOf("\n");
  for (const [signal, asker] of [...(askers.get(entry) ?? [])])
    if (!signal.aborted)
      requestProjectImage(entry.slice(0, at), entry.slice(at + 1), asker.read, signal, asker.decode, true);
}

/**
 * When the image read as `current` is read again: now, never (nothing changed
 * it), or in so many milliseconds. A write of the file or the viewed chat's
 * turn end reads it at once, but not twice within `PROJECT_IMAGE_LIMITS.again`;
 * another change, or a read that got no answer, once the recheck time since
 * the last read ends.
 */
function due(scope: string, entry: string, current: Marks): "now" | "fresh" | number {
  const since = performance.now() - current.at;
  if (current.touch !== (touches.get(entry) ?? 0) || current.urgent !== urgent(scope)) {
    const wait = PROJECT_IMAGE_LIMITS.again - since;
    return wait > 0 ? wait : "now";
  }
  if (current.epoch === epoch(scope) && !current.transient) return "fresh";
  const wait = PROJECT_IMAGE_LIMITS.recheck - since;
  return wait > 0 ? wait : "now";
}

/**
 * Asks for the image at `path` for a reply that shows it until `signal`
 * aborts (the reply unmounts). Nothing is read when nothing changed it since
 * it was read (see `due`), or when a read for it already runs or waits, which
 * the reply then waits on as well; a change that waits for the recheck time
 * leaves one read for when it ends. A read starts only while some reply that
 * asked is still there; it ends unsettled after `PROJECT_IMAGE_LIMITS.wait`,
 * and an answer after that changes nothing. Only an answer replaces what is
 * remembered: a file gone or no image says so, while a timeout or a dropped
 * connection keeps the picture and is read again on the next occasion.
 * `decode` checks that the window can draw the image (or says it is too
 * large) before it is shown.
 */
export function requestProjectImage(
  scope: string,
  path: string,
  read: ImageReader,
  signal: AbortSignal,
  decode: Decode = async () => true,
  again = false,
) {
  if (signal.aborted) return;
  const entry = key(scope, path);
  enlist(entry, signal, { read, decode });
  // A reply drawing it needs its picture; one showing it already asks again
  // on the terms its last read set, though its picture was let go for room.
  const current = remembered.get(entry) ?? (again ? reads.get(entry) : undefined);
  const when = current ? due(scope, entry, current) : "now";
  if (when === "fresh") return;
  if (when !== "now") {
    if (!trailing.has(entry))
      trailing.set(
        entry,
        setTimeout(() => {
          trailing.delete(entry);
          askAgain(entry);
        }, when + 5),
      );
    return;
  }
  const queued = pending.get(entry);
  if (queued) {
    queued.waiters.add(signal);
    return;
  }
  const job: Job = {
    waiters: new Set([signal]),
    start: () => {
      if ([...job.waiters].every((waiter) => waiter.aborted)) {
        if (pending.get(entry) === job) pending.delete(entry);
        return;
      }
      active++;
      // On the monotonic clock: a wall clock set back would hold reads off.
      const asked = { epoch: epoch(scope), urgent: urgent(scope), at: performance.now(), touch: touches.get(entry) ?? 0 };
      let timer: ReturnType<typeof setTimeout> | undefined;
      const deadline = new Promise<typeof UNSETTLED>(
        (done) => (timer = setTimeout(() => done(UNSETTLED), PROJECT_IMAGE_LIMITS.wait)),
      );
      const answer = Promise.resolve()
        .then(() => read(path))
        .then(
          async (file): Promise<ShownImage> => {
            const data = fileImage(file);
            if (!data) return { note: "failed" };
            if (tooManyPixels(file!.content!)) return { note: "large" };
            const size = file?.size ?? decodedSize(file!.content!);
            if (data === remembered.get(entry)?.data) return { data, size };
            const drawn = await decode(data);
            return drawn === true ? { data, size } : { note: drawn === "large" ? "large" : "failed" };
          },
          (error: unknown): ShownImage | typeof UNSETTLED =>
            tooLarge(error) ? { note: "large" } : refused(error) ? { note: "failed" } : UNSETTLED,
        )
        .catch((): typeof UNSETTLED => UNSETTLED);
      void Promise.race([answer, deadline])
        .then((shown) => {
          const before = remembered.get(entry);
          if (shown === UNSETTLED) {
            // Nothing was learned: what showed keeps showing, and is read again on the next occasion.
            if (before) remember(entry, { ...before, ...asked, transient: true });
            // Read before, its picture let go for room: replies showing it keep
            // it, and one drawn since, waiting for it, says it is not available.
            else if (reads.has(entry)) {
              marked(entry, { ...asked, transient: true });
              settle(scope, path, { note: "failed" });
            } else {
              remember(entry, { note: "failed", ...asked, transient: true });
              settle(scope, path, { note: "failed" });
            }
            return;
          }
          // A file gone or no longer an image says so from now on; a reply
          // already showing its picture keeps it (Markdown.tsx).
          remember(entry, { ...shown, ...asked });
          if (shown.data !== before?.data || shown.note !== before?.note) settle(scope, path, shown);
        })
        .finally(() => {
          clearTimeout(timer);
          if (pending.get(entry) === job) pending.delete(entry);
          active--;
          pump();
          // The project changed while it was read (a draw that joined it
          // wants the file after that change), or the read got no answer:
          // read again for the replies showing it, now or after the wait.
          askAgain(entry);
        });
    },
  };
  pending.set(entry, job);
  waiting.push(job);
  pump();
}

/** Forgets every remembered image and reads in waiting, as quitting does. */
export function forgetProjectImages() {
  remembered.clear();
  rememberedBytes = 0;
  reads.clear();
  epochs.clear();
  urgents.clear();
  touches.clear();
  writing.clear();
  for (const timer of trailing.values()) clearTimeout(timer);
  trailing.clear();
  askers.clear();
  local.clear();
  waiting.length = 0;
  pending.clear();
}

export interface ProjectImageLabels {
  /** Shown for an image with no alt text. */
  unnamed: string;
  outside: string;
  large: string;
  failed: string;
}

/**
 * Replaces each image under `root` whose source is a path with the image the
 * project already showed or a placeholder: its alt text, and a note when no
 * picture will come. `source` gives the address as the reply wrote it,
 * which the sanitizer may have taken off (a local `file:` URL). Paths the
 * placeholders wait on are read by Markdown.tsx.
 */
export function holdProjectImages(
  root: ParentNode,
  project: string | readonly string[],
  source: (image: Element) => string | null,
  shown: (path: string) => ShownImage | undefined,
  labels: ProjectImageLabels,
  within?: string,
) {
  for (const image of Array.from(root.querySelectorAll("img"))) {
    const written = source(image);
    const found = projectImagePath(written, project, within);
    if (!found) continue;
    const alt = image.getAttribute("alt") ?? "";
    const document = image.ownerDocument;
    if ("refused" in found) {
      image.replaceWith(projectPlaceholder(document, undefined, alt, labels, found.refused, written ?? ""));
      continue;
    }
    const now = shown(found.path);
    image.replaceWith(
      now?.data !== undefined
        ? projectImageElement(document, found.path, alt, now.data)
        : projectPlaceholder(document, found.path, alt, labels, now?.note, written ?? ""),
    );
  }
}

/** The image a placeholder stands for, once read. */
export function projectImageElement(document: Document, path: string, alt: string, data: string) {
  const image = document.createElement("img");
  image.className = "project-image-loaded";
  image.dataset.projectPath = path;
  image.alt = alt;
  image.src = data;
  markMade(image);
  return image;
}

/** A placeholder: the alt text, and the note when there is one. Without a path nothing is read for it. */
export function projectPlaceholder(
  document: Document,
  path: string | undefined,
  alt: string,
  labels: ProjectImageLabels,
  note: ImageNote | undefined,
  written: string,
) {
  const holder = document.createElement("span");
  holder.className = "project-image";
  if (path !== undefined) holder.dataset.projectPath = path;
  holder.dataset.projectAlt = alt;
  const text = document.createElement("span");
  text.className = "project-image-alt";
  text.textContent = alt || labels.unnamed;
  holder.append(text);
  if (note) noteImage(holder, note, labels, written);
  markMade(holder);
  return holder;
}

/** Says on a placeholder why no picture comes; the path as written is its tooltip. */
export function noteImage(holder: Element, note: ImageNote, labels: ProjectImageLabels, written?: string) {
  const document = holder.ownerDocument;
  let span = holder.querySelector(".project-image-note");
  if (!span) {
    span = document.createElement("span");
    span.className = "project-image-note";
    holder.append(span);
  }
  (span as HTMLElement).dataset.note = note;
  span.textContent = labels[note];
  if (written) holder.setAttribute("title", written);
}

/** Words the placeholders under `root` in the interface language again. */
export function relabelProjectImages(root: ParentNode, labels: ProjectImageLabels) {
  for (const holder of Array.from(root.querySelectorAll<HTMLElement>(".project-image"))) {
    const text = holder.querySelector(".project-image-alt");
    if (text) text.textContent = holder.dataset.projectAlt || labels.unnamed;
    const note = holder.querySelector<HTMLElement>(".project-image-note");
    const kind = note?.dataset.note as ImageNote | undefined;
    if (note && kind && kind in labels) note.textContent = labels[kind];
  }
}

/**
 * Asks for each of `paths` that a reply has not asked for yet (`asked`), up to
 * `PROJECT_IMAGE_LIMITS.paths` in all, and gives back the paths past that.
 */
export function askOnce(paths: readonly string[], asked: Set<string>, request: (path: string) => void) {
  const over = new Set<string>();
  for (const path of paths) {
    if (asked.has(path)) continue;
    if (asked.size >= PROJECT_IMAGE_LIMITS.paths) {
      over.add(path);
      continue;
    }
    asked.add(path);
    request(path);
  }
  return over;
}

// The window's side: these use the desktop bridge, so they run only in the app.

/** The project an image is read from, and whether a server event concerns the chat being viewed. */
export interface ImageProject {
  connection: string;
  /** The project's folders (`projectRoots`). */
  roots: string[];
  /** A remote host's project, whose files the window coming to the front does not change. */
  remote: boolean;
  viewed: (thread?: string | null, root?: string | null) => ReturnType<typeof inViewedTree>;
}

/** The project of the window's open chat, read from the app's state when an event arrives. */
export function imageProject(state: {
  connection?: { id: string; remote: boolean; workspace: string; info?: { workspace?: string | null } } | null;
  selected?: string | null;
  threads: readonly { id: string; parentId?: string | null }[];
}): ImageProject | undefined {
  const connection = state.connection;
  if (!connection) return undefined;
  return {
    connection: connection.id,
    roots: projectRoots(connection),
    remote: connection.remote,
    viewed: (thread, root) => inViewedTree(state.selected, state.threads, thread, root),
  };
}

let watching = false;
/** Each project's folders, which a tool's absolute path is read against. */
const roots = new Map<string, string[]>();
/** Images read from a project are read again once its files may have changed (see `projectEvent`). */
function watchProjects(viewed: ImageProject["viewed"]) {
  if (watching) return;
  watching = true;
  window.haarness.onEvent((event) => {
    if (event.kind !== "server" || !event.connection) return;
    applyProjectEvent(event.connection, event.data as Parameters<typeof projectEvent>[0], viewed, roots.get(event.connection));
  });
  window.addEventListener("focus", windowFocused);
}

/** Reads a project file for an image through the connection's file API. */
const reader =
  (connection: string): ImageReader =>
  (path) =>
    window.haarness.files<FileContent>(connection, "read", { path });

/**
 * Whether the window can draw the image, or "large" when it has more pixels
 * than `PROJECT_IMAGE_LIMITS.pixels`. Loading it reads only its header, which
 * gives its real size (a header the cheap check could not read, or one that
 * says less than the frames hold); only an image within the limit is then
 * decoded, which also lets it show at its size at once.
 */
async function decodeImage(data: string): Promise<boolean | "large"> {
  const image = new Image();
  const loaded = new Promise<boolean>((done) => {
    image.onload = () => done(true);
    image.onerror = () => done(false);
  });
  image.src = data;
  if (!(await loaded) || !image.naturalWidth) return false;
  if (image.naturalWidth * image.naturalHeight > PROJECT_IMAGE_LIMITS.pixels) return "large";
  try {
    await image.decode();
    return true;
  } catch {
    return false;
  }
}

/** Reads one image of the project for a view that shows it until `signal` aborts. */
export function readProjectImage(project: ImageProject, path: string, signal: AbortSignal) {
  watchProjects(project.viewed);
  roots.set(project.connection, project.roots);
  if (!project.remote) localProject(project.connection);
  requestProjectImage(project.connection, path, reader(project.connection), signal, decodeImage);
}

/**
 * Asks once for each project image `root` shows that this reply has not asked
 * for yet, for as long as `signal` is live: a live reply drawn again on every
 * piece of its text asks for each image once, and the settled answer reaches
 * it through `onProjectImageSettled`. A reply reads at most
 * `PROJECT_IMAGE_LIMITS.paths` paths; past them a placeholder says the image
 * is not available.
 */
export function readProjectImages(
  root: ParentNode,
  project: ImageProject | undefined,
  signal: AbortSignal,
  asked: Set<string>,
  labels: ProjectImageLabels,
) {
  if (!project) return;
  const elements = Array.from(root.querySelectorAll<HTMLElement>(".project-image, img.project-image-loaded")).filter(
    (element) => element.dataset.projectPath && madeHere(element),
  );
  const over = askOnce(
    elements.map((element) => element.dataset.projectPath!),
    asked,
    (path) => readProjectImage(project, path, signal),
  );
  for (const element of elements)
    if (over.has(element.dataset.projectPath!) && !(element instanceof HTMLImageElement))
      noteImage(element, "failed", labels, element.dataset.projectPath);
}
