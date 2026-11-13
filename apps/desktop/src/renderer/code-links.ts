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
 * which lists only inside the project or the chat's own folder, and keeps of
 * each listing only whether the names replies asked about are files there. A
 * few listings run at a time, and one reply looks up at most
 * `CODE_LINK_LIMITS.paths` paths. A folder is listed again on the occasions a
 * reply's images are read again (project-images.ts): at once after the chat
 * being viewed ends a turn or the file API wrote a file in it, and for any
 * other change once `CODE_LINK_LIMITS.recheck` has passed since its last
 * listing; a large folder, whose listing is costly over a remote link, at
 * most once in `CODE_LINK_LIMITS.largeRecheck`. Until a new listing answers,
 * the last answer stands, so a link never blinks.
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
 * folders remembered (the oldest goes first) and the names remembered in each,
 * how long a listing stands after a change that waits (and after a listing
 * that got no answer), the least time between two listings of a folder,
 * however often it changes, and the entries above which a folder is large,
 * with the time a large folder's listing stands whatever changes.
 */
export const CODE_LINK_LIMITS = {
  parallel: 3,
  wait: 30_000,
  paths: 20,
  folders: 256,
  names: 200,
  recheck: 20_000,
  again: 1_000,
  large: 2_000,
  largeRecheck: 60_000,
};

/** A file of the project (relative, `/`-separated) and the line to open it at. */
export type CodeReference = { path: string; line?: number };

/**
 * What inline code is never a file name with: a space, a quote, a backtick,
 * an operator, a glob, a variable, a query or a fragment, and, unless the
 * project is on a Windows drive, a backslash (an escape, a regular expression).
 */
const NOT_A_PATH = /[\s"'`<>{}()|;,*?$!=&^%#]/;

/**
 * The file and line inline code names, when it reads as a path inside the
 * project (one of `roots`) or the chat's own folder: no space or operator,
 * not a flag, a home path, a package or a web address, a colon only before
 * its line or after a drive letter, and a folder in it or a name with an
 * extension (`.env`, `main.rs`; `1.2.3`, `v2.0` and `Makefile` are not). In a
 * project on a Windows drive, `src\a.ts` and `C:\proj\src\a.ts` count too.
 * Whether the file is there is looked up separately.
 */
export function codeReference(code: string, roots: readonly string[]): CodeReference | undefined {
  const text = code.trim();
  const windows = roots.some((root) => /^[a-z]:[\\/]/i.test(root));
  if (!text || text.length > 300 || NOT_A_PATH.test(text) || /^[-~@+]/.test(text)) return undefined;
  if (!windows && text.includes("\\")) return undefined;
  const line = /:\d+(?:[:-]\d+)?$/.exec(text);
  const path = line ? text.slice(0, line.index) : text;
  if (!path || /[\\/]$/.test(path) || path.replace(/^[a-z]:[\\/]/i, "").includes(":")) return undefined;
  const name = path.split(/[\\/]/).at(-1)!;
  if (!/[\\/]/.test(path) && !/\.[\w-]*[a-z][\w-]*$/i.test(name)) return undefined;
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
 * A listing: whether each name asked about is a file in the folder, how many
 * entries it had, the project's changes it was taken after (those that wait
 * for the recheck time and those that do not, and the file API's writes in
 * the folder), and when. `transient` marks one that got no answer (a timeout,
 * a dropped connection): it is taken again once the recheck time after it
 * ended has passed.
 */
type Marks = { epoch: number; urgent: number; touch: number; at: number; transient?: boolean };
type Listing = Marks & { answers: ReadonlyMap<string, boolean>; size: number };
/** A folder: the names replies asked about in it, and its last listing. */
type Folder = { names: Set<string>; listing?: Listing };

const folders = new Map<string, Folder>();
/** The projects asked about, which a reconnect marks as changed. */
const scopes = new Set<string>();
const epochs = new Map<string, number>();
const urgents = new Map<string, number>();
const touches = new Map<string, number>();
const key = (scope: string, folder: string) => `${scope}\n${folder}`;
const epoch = (scope: string) => epochs.get(scope) ?? 0;
const urgent = (scope: string) => urgents.get(scope) ?? 0;
const touch = (entry: string) => touches.get(entry) ?? 0;

/** The folder kept under `entry`, made the newest; the oldest go when there are too many. */
function folderAt(entry: string) {
  const found = folders.get(entry) ?? { names: new Set<string>() };
  folders.delete(entry);
  folders.set(entry, found);
  for (const oldest of folders.keys()) {
    if (folders.size <= CODE_LINK_LIMITS.folders) break;
    folders.delete(oldest);
  }
  return found;
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

/** The connections that are up, whose projects' folders may be listed. */
const up = new Set<string>();
/** The connection a project (or a chat's own folder) is reached through. */
const connectionOf = (scope: string) => scope.split("\n")[0];
/** The lister each project was last asked through, which a listing uses when it starts. */
const listers = new Map<string, FolderLister>();

/**
 * Whether `connection` is up. While it is down nothing of its projects is
 * listed: a listing due then is dropped, and the replies waiting keep their
 * last answers. Once it is back (connected again, or caught up after a gap
 * in its events), its projects' files may have changed meanwhile, so their
 * folders are listed again as after a turn, a listing that got no answer
 * included, and the replies on screen look again.
 */
export function codeLinksConnection(connection: string, connected: boolean) {
  if (!connected) {
    up.delete(connection);
    return;
  }
  if (up.has(connection)) return;
  up.add(connection);
  for (const scope of scopes) {
    if (scope !== connection && !scope.startsWith(connection + "\n")) continue;
    urgents.set(scope, urgent(scope) + 1);
    for (const [entry, folder] of folders)
      if (entry.startsWith(scope + "\n") && folder.listing?.transient)
        folder.listing = { ...folder.listing, transient: false };
    tell(scope);
  }
}

/**
 * When the folder listed as `listing` is listed again: now, never (nothing
 * changed it), or in so many milliseconds. A listing that got no answer, once
 * the recheck time since it ended has passed. A large folder, once
 * `CODE_LINK_LIMITS.largeRecheck` has passed, whatever changed or was asked.
 * Otherwise a name asked about that it has no answer for, a file API write in
 * it or the viewed chat's turn end lists it at once, though not twice within
 * `CODE_LINK_LIMITS.again`, and another change once the recheck time since the
 * last listing ends.
 */
function due(scope: string, entry: string, listing: Listing, missing: boolean): "fresh" | "now" | number {
  const since = performance.now() - listing.at;
  const after = (wait: number) => (wait - since > 0 ? wait - since : "now");
  if (listing.transient) return after(CODE_LINK_LIMITS.recheck);
  const urgentChange = missing || listing.touch !== touch(entry) || listing.urgent !== urgent(scope);
  const change = urgentChange || listing.epoch !== epoch(scope);
  if (!change) return "fresh";
  if (listing.size > CODE_LINK_LIMITS.large) return after(CODE_LINK_LIMITS.largeRecheck);
  return after(urgentChange ? CODE_LINK_LIMITS.again : CODE_LINK_LIMITS.recheck);
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
/** The listing each folder waits for until its time (`due`) comes, with the replies waiting on it. */
const later = new Map<string, { timer: ReturnType<typeof setTimeout>; due: number; waiters: Set<AbortSignal> }>();

/** Starts the listings that wait, a few at a time; one whose replies have all gone is never started. */
function pump() {
  while (active < CODE_LINK_LIMITS.parallel && waiting.length) waiting.shift()!.start();
}

/** Whether the two answer the same: the same names are files. */
const same = (a: ReadonlyMap<string, boolean> | undefined, b: ReadonlyMap<string, boolean>) =>
  [...new Set([...(a?.keys() ?? []), ...b.keys()])].every((name) => (a?.get(name) === true) === (b.get(name) === true));

/**
 * Lists `folder` of project `scope` for a reply that waits until `signal`
 * ends, when `due` says so, and only while its connection is up.
 */
function ask(scope: string, folder: string, signal: AbortSignal) {
  if (signal.aborted || !up.has(connectionOf(scope))) return;
  const entry = key(scope, folder);
  const known = folders.get(entry);
  const current = known?.listing;
  const missing = !!current && [...(known?.names ?? [])].some((name) => !current.answers.has(name));
  const when = current ? due(scope, entry, current, missing) : "now";
  if (when === "fresh") return;
  const queued = pending.get(entry);
  if (queued) {
    queued.waiters.add(signal);
    return;
  }
  const waited = later.get(entry);
  if (when !== "now") {
    // One wait per folder, ending at the soonest time asked for: a change
    // that lists at once is not held back by one that waits.
    const at = performance.now() + when;
    if (waited && waited.due <= at) {
      waited.waiters.add(signal);
      return;
    }
    if (waited) clearTimeout(waited.timer);
    const waiters = new Set([...(waited?.waiters ?? []), signal]);
    later.set(entry, {
      waiters,
      due: at,
      timer: setTimeout(() => {
        later.delete(entry);
        for (const waiter of waiters) ask(scope, folder, waiter);
      }, when + 5),
    });
    return;
  }
  // Listed now: a wait for later has nothing left to do.
  if (waited) {
    clearTimeout(waited.timer);
    later.delete(entry);
  }
  const job: Job = {
    waiters: new Set([signal, ...(waited?.waiters ?? [])]),
    start: () => {
      // The lister as the replies last gave it, and only while the connection is up.
      const list = listers.get(scope);
      if ([...job.waiters].every((waiter) => waiter.aborted) || !list || !up.has(connectionOf(scope))) {
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
          (listed) => listed?.entries ?? [],
          (error: unknown) => (noFolder(error) ? [] : UNANSWERED),
        )
        .catch((): typeof UNANSWERED => UNANSWERED);
      void Promise.race([answer, deadline])
        .then((entries) => {
          const kept = folderAt(entry);
          const before = kept.listing;
          if (entries === UNANSWERED) {
            // Nothing was learned: the last answer stands, and the folder is
            // listed again once the recheck time after this attempt is up.
            kept.listing = {
              ...asked,
              at: performance.now(),
              answers: before?.answers ?? new Map(),
              size: before?.size ?? 0,
              transient: true,
            };
            return;
          }
          // A link opens only a file the project holds itself: a folder, and
          // a link that may lead out of the project, are not linked. Only the
          // names asked about are kept.
          const files = new Set(entries.filter((item) => !item.directory && !item.symlink).map((item) => item.name));
          const answers = new Map([...kept.names].map((name) => [name, files.has(name)] as const));
          kept.listing = { ...asked, answers, size: entries.length };
          if (!same(before?.answers, answers)) tell(scope);
        })
        .finally(() => {
          clearTimeout(timer);
          if (pending.get(entry) === job) pending.delete(entry);
          active--;
          pump();
          // The project changed while it was listed, a name was asked about
          // meanwhile, or the listing got no answer: list it again for the
          // replies still waiting, when due.
          for (const waiter of job.waiters) ask(scope, folder, waiter);
        });
    },
  };
  pending.set(entry, job);
  waiting.push(job);
  pump();
}

/**
 * Whether `path` names a file of project `scope`, as far as is known: from
 * the last listing of its folder, or undefined before any answer for it.
 * Lists the folder through `list` (again) when it has no answer for the name
 * yet or the project changed since, as `due` says, while the connection is
 * up (`codeLinksConnection`); `signal` ends when the reply asking is gone, so
 * a listing only it waited for never starts.
 */
export function codeFileKnown(scope: string, path: string, list: FolderLister, signal: AbortSignal) {
  const { folder, name } = fileFolder(path);
  const kept = folderAt(key(scope, folder));
  kept.names.delete(name);
  kept.names.add(name);
  for (const oldest of kept.names) {
    if (kept.names.size <= CODE_LINK_LIMITS.names) break;
    kept.names.delete(oldest);
  }
  scopes.add(scope);
  listers.set(scope, list);
  const known = kept.listing?.answers.get(name);
  ask(scope, folder, signal);
  return known;
}

/** Forgets every listing, and listings in waiting. */
export function forgetCodeLinks() {
  folders.clear();
  scopes.clear();
  up.clear();
  listers.clear();
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
