/*
 * Inline code in the conversation that names a file of the project, with or
 * without a line (`src/app.ts`, `src/app.ts:42`, `a.ts:12-20`, an absolute
 * path inside the project), opens that file in the Files panel at its line,
 * as a link to it does (link-target.ts, whose rules decide what is inside the
 * project or a chat's own folder). Replies name files this way far more often
 * than they link them.
 *
 * Only a file that is there becomes a link, so code that merely reads like a
 * path (`console.log`, a file the reply only plans to write) stays as it is.
 * The app lists the folder of each file named, through the server's file API,
 * which lists only inside the project or the chat's own folder. Each listing
 * is remembered per project and folder, a few run at a time, and one reply
 * looks up at most `CODE_LINK_LIMITS.paths` paths. A folder is listed again
 * on the occasions a reply's images are read again (project-images.ts): at
 * once after the chat being viewed ends a turn or the file API wrote a file
 * in it, and once `CODE_LINK_LIMITS.recheck` has passed since its last
 * listing for any other change. Until a new listing answers, the last answer
 * stands, so a link never blinks.
 *
 * The code keeps its own element and box: it only gains the link's role, a
 * tab stop and a class, so nothing moves when it becomes a link. Which code
 * is a link, and what it opens, the app keeps to itself; a reply's markup
 * keeps no role, tab stop or class of its own (markdown-dom.ts), so it cannot
 * pass for one. Pure apart from the DOM it is handed and the lister it is
 * given, so it is tested without a window.
 */
import { linkTarget } from "./link-target";
import { onProjectChange, type ProjectFilesChange } from "./project-images";
import type { FileEntry } from "../shared";

/**
 * The listings that run at once across every reply, how long one may run (in
 * milliseconds) before it ends unanswered, the paths one reply looks up, the
 * folders whose listing is remembered (the oldest goes first), how long a
 * listing stands after a change that waits, and the least time between two
 * listings of a folder, however often it changes.
 */
export const CODE_LINK_LIMITS = {
  parallel: 3,
  wait: 30_000,
  paths: 20,
  folders: 256,
  recheck: 20_000,
  again: 1_000,
};

/** A file of the project (relative, `/`-separated) and the line to open it at. */
export type CodeReference = { path: string; line?: number };

/**
 * What inline code is never a file name with: a space, a quote, a backtick,
 * an operator, a glob, a variable, a backslash escape, a query or a fragment.
 */
const NOT_A_PATH = /[\s"'`<>{}()|;,*?$!=&^%\\#]/;

/**
 * The file and line inline code names, when it reads as a path inside the
 * project (one of `roots`) or the chat's own folder: no space or operator,
 * not a flag, a home path, a package or a web address, a colon only before
 * its line or after a drive letter, and a folder in it or a name with an
 * extension (`.env`, `main.rs`; `1.2.3`, `v2.0` and `Makefile` are not).
 * Whether the file is there is looked up separately.
 */
export function codeReference(code: string, roots: readonly string[]): CodeReference | undefined {
  const text = code.trim();
  if (!text || text.length > 300 || NOT_A_PATH.test(text) || /^[-~@+]/.test(text)) return undefined;
  const line = /:\d+(?:[:-]\d+)?$/.exec(text);
  const path = line ? text.slice(0, line.index) : text;
  if (!path || path.endsWith("/") || path.replace(/^[a-z]:\//i, "").includes(":")) return undefined;
  const name = path.slice(path.lastIndexOf("/") + 1);
  if (!path.includes("/") && !/\.[\w-]*[a-z][\w-]*$/i.test(name)) return undefined;
  for (const root of roots) {
    if (!root) continue;
    const target = linkTarget(text, { root });
    if (target.kind === "file" && target.path)
      return target.line ? { path: target.path, line: target.line } : { path: target.path };
  }
  return undefined;
}

/** The folder a project file is in ("" at the top) and its name. */
export function fileFolder(path: string) {
  const at = path.lastIndexOf("/");
  return at < 0 ? { folder: "", name: path } : { folder: path.slice(0, at), name: path.slice(at + 1) };
}

/** A folder's entries as the file API lists them. */
export type FolderLister = (
  folder: string,
) => Promise<{ entries?: readonly { name: string; directory?: boolean; symlink?: boolean }[] } | undefined>;

/**
 * A listing: the files in the folder (undefined before any answer), the
 * project's changes it was taken after (those that wait for the recheck time
 * and those that do not, and the file API's writes in the folder), and when.
 * `transient` marks one that got no answer (a timeout, a dropped connection):
 * it is taken again as after a change.
 */
type Marks = { epoch: number; urgent: number; touch: number; at: number; transient?: boolean };
type Listing = Marks & { files?: ReadonlySet<string> };

const listings = new Map<string, Listing>();
const epochs = new Map<string, number>();
const urgents = new Map<string, number>();
const touches = new Map<string, number>();
const key = (scope: string, folder: string) => `${scope}\n${folder}`;
const epoch = (scope: string) => epochs.get(scope) ?? 0;
const urgent = (scope: string) => urgents.get(scope) ?? 0;
const touch = (entry: string) => touches.get(entry) ?? 0;

function remember(entry: string, listing: Listing) {
  listings.delete(entry);
  listings.set(entry, listing);
  for (const oldest of listings.keys()) {
    if (listings.size <= CODE_LINK_LIMITS.folders) break;
    listings.delete(oldest);
  }
}

type Changed = (scope: string) => void;
const listeners = new Set<Changed>();

/**
 * Calls `listener` with a project's scope when the files its replies' inline
 * code names may have changed: a listing answered differently, or the project
 * changed, so each reply on screen looks its code up again.
 */
export function onCodeLinksChanged(listener: Changed): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function tell(scope: string) {
  for (const listener of [...listeners]) {
    try {
      listener(scope);
    } catch {
      /* One reply's trouble never keeps the others from hearing. */
    }
  }
}

/** A change of the project's files, as project-images.ts hears it. */
export function codeFilesChanged(scope: string, change: ProjectFilesChange) {
  if (change.path !== undefined) {
    const entry = key(scope, fileFolder(change.path).folder);
    touches.set(entry, touch(entry) + 1);
  } else if (change.now) urgents.set(scope, urgent(scope) + 1);
  else epochs.set(scope, epoch(scope) + 1);
  tell(scope);
}
onProjectChange(codeFilesChanged);

/**
 * When the folder listed as `listing` is listed again: now, never (nothing
 * changed it), or in so many milliseconds. A file API write in it or the
 * viewed chat's turn end lists it at once, but not twice within
 * `CODE_LINK_LIMITS.again`; another change, or a listing that got no answer,
 * once the recheck time since the last listing ends.
 */
function due(scope: string, entry: string, listing: Listing): "fresh" | "now" | number {
  const since = performance.now() - listing.at;
  if (listing.touch !== touch(entry) || listing.urgent !== urgent(scope)) {
    const wait = CODE_LINK_LIMITS.again - since;
    return wait > 0 ? wait : "now";
  }
  if (listing.epoch === epoch(scope) && !listing.transient) return "fresh";
  const wait = CODE_LINK_LIMITS.recheck - since;
  return wait > 0 ? wait : "now";
}

/**
 * Whether the server answered that the folder is not there or not one it
 * lists (gone, outside the project, Git metadata, a file), which is an
 * answer, unlike a timeout or a dropped connection.
 */
const noFolder = (error: unknown) =>
  /no longer exists|not found|escapes the workspace|must be relative|Git metadata|not a directory/i.test(
    error instanceof Error ? error.message : String(error),
  );

const UNANSWERED = Symbol("unanswered");
type Job = { waiters: Set<AbortSignal>; start: () => void };
const pending = new Map<string, Job>();
const waiting: Job[] = [];
let active = 0;
/** The listing each folder waits for until its time comes, with the replies waiting on it. */
const later = new Map<string, { timer: ReturnType<typeof setTimeout>; waiters: Set<AbortSignal> }>();

/** Starts the listings that wait, a few at a time; one whose replies have all gone is never started. */
function pump() {
  while (active < CODE_LINK_LIMITS.parallel && waiting.length) waiting.shift()!.start();
}

const same = (a: ReadonlySet<string> | undefined, b: ReadonlySet<string> | undefined) =>
  (a?.size ?? 0) === (b?.size ?? 0) && [...(a ?? [])].every((name) => b?.has(name));

/** Lists `folder` of project `scope` for a reply that waits until `signal` ends, when `due` says so. */
function ask(scope: string, folder: string, list: FolderLister, signal: AbortSignal) {
  if (signal.aborted) return;
  const entry = key(scope, folder);
  const current = listings.get(entry);
  const when = current ? due(scope, entry, current) : "now";
  if (when === "fresh") return;
  const queued = pending.get(entry);
  if (queued) {
    queued.waiters.add(signal);
    return;
  }
  if (when !== "now") {
    let wait = later.get(entry);
    if (!wait) {
      const waiters = new Set<AbortSignal>();
      wait = {
        waiters,
        timer: setTimeout(() => {
          later.delete(entry);
          for (const waiter of waiters) ask(scope, folder, list, waiter);
        }, when + 5),
      };
      later.set(entry, wait);
    }
    wait.waiters.add(signal);
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
      // On the monotonic clock: a wall clock set back would hold listings off.
      const asked: Marks = { epoch: epoch(scope), urgent: urgent(scope), touch: touch(entry), at: performance.now() };
      let timer: ReturnType<typeof setTimeout> | undefined;
      const deadline = new Promise<typeof UNANSWERED>(
        (done) => (timer = setTimeout(() => done(UNANSWERED), CODE_LINK_LIMITS.wait)),
      );
      const answer = Promise.resolve()
        .then(() => list(folder))
        .then(
          (listed): ReadonlySet<string> =>
            // A link opens only a file the project holds itself: a folder,
            // and a link that may lead out of the project, are not linked.
            new Set((listed?.entries ?? []).filter((item) => !item.directory && !item.symlink).map((item) => item.name)),
          (error: unknown) => (noFolder(error) ? new Set<string>() : UNANSWERED),
        )
        .catch((): typeof UNANSWERED => UNANSWERED);
      void Promise.race([answer, deadline])
        .then((files) => {
          const before = listings.get(entry);
          if (files === UNANSWERED) {
            // Nothing was learned: the last answer stands, and is asked again on the next occasion.
            remember(entry, { ...asked, files: before?.files, transient: true });
            return;
          }
          remember(entry, { ...asked, files });
          if (!same(before?.files, files)) tell(scope);
        })
        .finally(() => {
          clearTimeout(timer);
          if (pending.get(entry) === job) pending.delete(entry);
          active--;
          pump();
          // The project changed while it was listed, or the listing got no
          // answer: list it again for the replies still waiting, when due.
          for (const waiter of job.waiters) ask(scope, folder, list, waiter);
        });
    },
  };
  pending.set(entry, job);
  waiting.push(job);
  pump();
}

/**
 * Whether `path` names a file of project `scope`, as far as is known: from
 * the last listing of its folder, or undefined before any answer. Lists the
 * folder (again) when it has no listing yet or the project changed since, as
 * `due` says; `signal` ends when the reply asking is gone, so a listing only
 * it waited for never starts.
 */
export function codeFileKnown(scope: string, path: string, list: FolderLister, signal: AbortSignal) {
  const { folder, name } = fileFolder(path);
  const known = listings.get(key(scope, folder))?.files?.has(name);
  ask(scope, folder, list, signal);
  return known;
}

/** Forgets every listing, and listings in waiting. */
export function forgetCodeLinks() {
  listings.clear();
  epochs.clear();
  urgents.clear();
  touches.clear();
  for (const wait of later.values()) clearTimeout(wait.timer);
  later.clear();
  waiting.length = 0;
  pending.clear();
}

/** The class of inline code the app made a link. */
export const CODE_LINK = "code-link";
/** The file each code link the app made opens. */
const linked = new WeakMap<Element, CodeReference>();

/** The file a code link the app made opens; undefined for anything else, a reply's own markup included. */
export function codeLinkTarget(element: Element | null | undefined): CodeReference | undefined {
  return element?.classList.contains(CODE_LINK) ? linked.get(element) : undefined;
}

export interface CodeLinkPass {
  /** The project's folders (`projectRoots`), or the chat's own folder. */
  roots: readonly string[];
  /** Whether a path names a file, as far as is known (`codeFileKnown`). */
  known: (path: string) => boolean | undefined;
  /** The paths this reply looked up; past `CODE_LINK_LIMITS.paths`, code stays as written. */
  asked: Set<string>;
  /** A reply still being written links nothing, so nothing changes under the pointer. */
  live: boolean;
}

/**
 * Makes each inline code under `root` that names a file of the project a link
 * to it, and each that no longer does (its file gone, the reply being written
 * again) plain code again. Code in a code block, a link, a summary or a button
 * stays as it is. A path whose file is not known yet stays as it is until its
 * listing answers (`onCodeLinksChanged`).
 */
export function linkCodeSpans(root: ParentNode, pass: CodeLinkPass) {
  for (const code of Array.from(root.querySelectorAll("code"))) {
    const reference = pass.live || code.closest("pre, a, summary, button") ? undefined : looked(code, pass);
    if (reference) {
      linked.set(code, reference);
      code.classList.add(CODE_LINK);
      code.setAttribute("role", "link");
      code.setAttribute("tabindex", "0");
    } else if (linked.has(code)) {
      linked.delete(code);
      code.classList.remove(CODE_LINK);
      if (!code.classList.length) code.removeAttribute("class");
      code.removeAttribute("role");
      code.removeAttribute("tabindex");
    }
  }
}

function looked(code: Element, pass: CodeLinkPass) {
  const reference = codeReference(code.textContent ?? "", pass.roots);
  if (!reference) return undefined;
  if (!pass.asked.has(reference.path)) {
    if (pass.asked.size >= CODE_LINK_LIMITS.paths) return undefined;
    pass.asked.add(reference.path);
  }
  const known = pass.known(reference.path);
  // Not known yet (its listing let go for room): a link stays one meanwhile.
  if (known === undefined) return linked.get(code)?.path === reference.path ? reference : undefined;
  return known ? reference : undefined;
}

// The window's side: these use the window and the desktop bridge, so they run only in the app.

/**
 * A pointer went down on a code link. A tab stop takes a press for focus, and
 * the window then starts no selection in it, so its path could not be dragged
 * over to copy part of it, as plain code can. So for as long as the press
 * lasts the code has no tab stop: the press finds text, and the keyboard's
 * tab stop comes back when the press ends.
 */
export function pressCodeLink(code: Element) {
  if (!codeLinkTarget(code)) return;
  code.removeAttribute("tabindex");
  const ended = () => {
    window.removeEventListener("pointerup", ended, true);
    window.removeEventListener("pointercancel", ended, true);
    if (codeLinkTarget(code)) code.setAttribute("tabindex", "0");
  };
  window.addEventListener("pointerup", ended, true);
  window.addEventListener("pointercancel", ended, true);
}

/** Lists a folder of the project, or of a chat's own folder, through the connection's file API. */
export const folderLister =
  (connection: string, threadId?: string): FolderLister =>
  (folder) =>
    window.haarness.files<{ entries: FileEntry[] }>(connection, "list", {
      path: folder,
      hidden: true,
      ...(threadId ? { threadId } : {}),
    });
