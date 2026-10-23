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
 * What was read is remembered for the project in memory, so a reply drawn
 * again (scrolled back into view, or the live reply once it is saved) shows
 * its image at once, with no placeholder in between. Once the project may
 * have changed (a turn ended, a tool wrote files, the window came back to the
 * front) a remembered image still shows at once, is read again behind it, and
 * is replaced only when the file changed. Pure apart from the DOM it is
 * handed, so it is tested without a window.
 */
import { linkTarget, parsedAddress } from "./link-target";
import { madeHere, markMade } from "./remote-images";
import type { FileContent } from "../shared";

/**
 * The reads that run at once across every reply, how long one may run (in
 * milliseconds) before it ends as unavailable, the paths one reply reads, and
 * the image data remembered across all projects (the oldest goes first). The
 * server reads files of at most 10 MiB.
 */
export const PROJECT_IMAGE_LIMITS = { parallel: 3, wait: 30_000, paths: 20, bytes: 64 * 1024 * 1024 };

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
 * What an image source names, decided before anything is read. Undefined
 * when it is not a path at all (a remote address, `data:`, `blob:`, a
 * fragment, a network path), which stays as the sanitizer left it. A path
 * that leaves the project, or a local `file:` URL there, is refused as
 * outside; one that is not a raster image, or is spelled with a control
 * character, as unreadable.
 */
export function projectImagePath(src: string | null | undefined, root: string): ProjectImagePath | undefined {
  const raw = parsedAddress(src ?? "");
  if (!raw || raw.startsWith("#") || /^[\\/]{2}/.test(raw)) return undefined;
  const scheme = /^([a-z][a-z\d+.-]*):/i.exec(raw)?.[1]?.toLowerCase();
  const drive = scheme?.length === 1 && /^[a-z]:[\\/]/i.test(raw);
  if (scheme && !drive && scheme !== "file") return undefined;
  if (!root) return { refused: "outside" };
  const target = linkTarget(raw, { root });
  if (target.kind !== "file") return { refused: "outside" };
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

/** The first 48 bytes of base64 text, enough to tell an image's type, without decoding the rest. */
function head(base64: string) {
  try {
    return Uint8Array.from(atob(base64.slice(0, 64)), (char) => char.charCodeAt(0));
  } catch {
    return new Uint8Array();
  }
}

/**
 * A file the server returned, as an image to show: a data URL of the type
 * its bytes start as, or undefined when it is text or no image the window
 * draws.
 */
export function fileImage(file: { content?: string; encoding?: string } | undefined): string | undefined {
  if (!file?.content || file.encoding !== "base64") return undefined;
  const type = sniffImage(head(file.content));
  return type ? `data:${type};base64,${file.content}` : undefined;
}

/** Reads one project file through the server's file API. */
export type ImageReader = (path: string) => Promise<{ content?: string; encoding?: string } | undefined>;

/** What a reply shows for a path: the image, or why there is none. */
export type ShownImage = { data: string; note?: undefined } | { data?: undefined; note: ImageNote };

type Entry = ShownImage & { epoch: number };

const remembered = new Map<string, Entry>();
let rememberedBytes = 0;
/** Each project's changes, and every project's (a window brought back to the front). */
const epochs = new Map<string, number>();
let everywhere = 0;
const key = (scope: string, path: string) => `${scope}\n${path}`;
const epoch = (scope: string) => (epochs.get(scope) ?? 0) + everywhere;

function remember(entry: string, value: Entry) {
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
  return entry.data !== undefined ? { data: entry.data } : { note: entry.note! };
}

/**
 * The project may have changed on disk: what it showed keeps showing, and is
 * read again the next time a reply draws it. Without a scope, every project.
 */
export function projectChanged(scope?: string) {
  if (scope === undefined) everywhere++;
  else epochs.set(scope, (epochs.get(scope) ?? 0) + 1);
}

/**
 * Whether a server event means files in its project may have changed: the
 * file API wrote one, a turn ended, or a tool that writes finished (as the
 * Files panel reads them).
 */
export function changesProject(event: { type?: string; payload?: unknown }) {
  if (event.type === "files.updated" || event.type === "turn.finished") return true;
  if (event.type !== "item.created") return false;
  const data = (event.payload as { data?: { type?: string; name?: string; is_error?: boolean } } | undefined)?.data;
  return (
    data?.type === "tool_result" && !data.is_error && /write|edit|patch|apply|bash|shell|move|delete/.test(data.name ?? "")
  );
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
 * Asks for the image at `path` for a reply that shows it until `signal`
 * aborts (the reply unmounts). Nothing is read when this project shows it
 * since its latest change, or a read for it already runs or waits, which the
 * reply then waits on as well. A read starts only while some reply that
 * asked is still there; it ends as unavailable after
 * `PROJECT_IMAGE_LIMITS.wait`, and an answer after that changes nothing.
 * `decode` checks that the window can draw the image before it is shown.
 */
export function requestProjectImage(
  scope: string,
  path: string,
  read: ImageReader,
  signal: AbortSignal,
  decode: (data: string) => Promise<boolean> = async () => true,
) {
  if (signal.aborted) return;
  const entry = key(scope, path);
  const current = remembered.get(entry);
  if (current && current.epoch === epoch(scope)) return;
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
      const asked = epoch(scope);
      let timer: ReturnType<typeof setTimeout> | undefined;
      const deadline = new Promise<"late">((done) => (timer = setTimeout(() => done("late"), PROJECT_IMAGE_LIMITS.wait)));
      const answer = Promise.resolve()
        .then(() => read(path))
        .then(
          async (file): Promise<ShownImage> => {
            const data = fileImage(file);
            if (!data) return { note: "failed" };
            if (data === remembered.get(entry)?.data) return { data };
            return (await decode(data)) ? { data } : { note: "failed" };
          },
          (error: unknown): ShownImage => ({ note: tooLarge(error) ? "large" : "failed" }),
        );
      void Promise.race([answer, deadline])
        .then((result) => {
          const shown: ShownImage = result === "late" ? { note: "failed" } : result;
          const before = remembered.get(entry);
          // A file that cannot be read again keeps the picture it showed.
          const kept = shown.note && before?.data !== undefined ? { data: before.data } : shown;
          remember(entry, { ...kept, epoch: asked });
          if (kept.data !== before?.data || kept.note !== before?.note) settle(scope, path, kept);
        })
        .finally(() => {
          clearTimeout(timer);
          if (pending.get(entry) === job) pending.delete(entry);
          active--;
          pump();
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
  epochs.clear();
  everywhere = 0;
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
  project: string,
  source: (image: Element) => string | null,
  shown: (path: string) => ShownImage | undefined,
  labels: ProjectImageLabels,
) {
  for (const image of Array.from(root.querySelectorAll("img"))) {
    const written = source(image);
    const found = projectImagePath(written, project);
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

// The window's side: these use the desktop bridge, so they run only in the app.

let watching = false;
/**
 * Images read from a project are read again once its files may have changed:
 * a turn ended, a tool wrote files, or the window came back to the front,
 * where the person may have changed them in another app.
 */
function watchProjects() {
  if (watching) return;
  watching = true;
  window.haarness.onEvent((event) => {
    if (event.kind === "server" && changesProject(event.data as { type?: string; payload?: unknown }))
      projectChanged(event.connection);
  });
  window.addEventListener("focus", () => projectChanged());
}

/** Reads a project file for an image through the connection's file API. */
const reader =
  (connection: string): ImageReader =>
  (path) =>
    window.haarness.files<FileContent>(connection, "read", { path });

/** Whether the window can draw the image; decoding it first also lets it show at its size at once. */
async function decodeImage(data: string) {
  const image = new Image();
  image.src = data;
  try {
    await image.decode();
    return image.naturalWidth > 0;
  } catch {
    return false;
  }
}

/** Reads one image of the connection's project for a view that shows it until `signal` aborts. */
export function readProjectImage(connection: string, path: string, signal: AbortSignal) {
  watchProjects();
  requestProjectImage(connection, path, reader(connection), signal, decodeImage);
}

/**
 * Reads each project image `root` shows that its project has not shown
 * since its files last changed, for as long as `signal` is live. A reply
 * reads at most `PROJECT_IMAGE_LIMITS.paths` paths; past them a placeholder
 * says the image is not available.
 */
export function readProjectImages(
  root: ParentNode,
  connection: string | undefined,
  signal: AbortSignal,
  asked: Set<string>,
  labels: ProjectImageLabels,
) {
  if (!connection) return;
  for (const element of Array.from(root.querySelectorAll<HTMLElement>(".project-image, img.project-image-loaded"))) {
    const path = element.dataset.projectPath;
    if (!path || !madeHere(element)) continue;
    if (!asked.has(path) && asked.size >= PROJECT_IMAGE_LIMITS.paths) {
      if (!(element instanceof HTMLImageElement)) noteImage(element, "failed", labels, path);
      continue;
    }
    asked.add(path);
    readProjectImage(connection, path, signal);
  }
}
