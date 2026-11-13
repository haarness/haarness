import test, { afterEach, beforeEach } from "node:test";
import assert from "node:assert/strict";
import {
  CODE_LINK,
  CODE_LINK_LIMITS,
  codeFileKnown,
  codeLinksConnection,
  codeLinkTarget,
  codeReference,
  fileFolder,
  forgetCodeLinks,
  linkCodeSpans,
  onCodeLinksChanged,
  type FolderLister,
} from "../src/renderer/code-links";
import { projectChanged, projectFileWritten } from "../src/renderer/project-images";

const root = "/Users/me/work/app";
const LIMITS = { ...CODE_LINK_LIMITS };
// The reply each test draws, gone when the test ends, so no listing outlives it.
let reply = new AbortController();
beforeEach(() => {
  reply = new AbortController();
  // The connections these tests list through are up.
  for (const connection of ["p", "q", "c1", "c2"]) codeLinksConnection(connection, true);
  // A listing held past its test ends unanswered soon after.
  CODE_LINK_LIMITS.wait = 300;
});
afterEach(() => {
  reply.abort();
  forgetCodeLinks();
  Object.assign(CODE_LINK_LIMITS, LIMITS);
});
const settle = () => new Promise((done) => setTimeout(done, 0));
const sleep = (ms: number) => new Promise((done) => setTimeout(done, ms));

test("inline code that reads as a path in the project names its file and line", () => {
  for (const [code, path, line] of [
    ["src/a.ts", "src/a.ts"],
    ["src/a.ts:42", "src/a.ts", 42],
    [" src/a.ts:42 ", "src/a.ts", 42],
    ["./a.ts", "a.ts"],
    ["a.ts:4-9", "a.ts", 4],
    ["a.ts:12:4", "a.ts", 12],
    ["README.md:3", "README.md", 3],
    ["main.rs", "main.rs"],
    ["package.json", "package.json"],
    [".env", ".env"],
    ["scripts/build", "scripts/build"],
    ["app/[id]/page.tsx", "app/[id]/page.tsx"],
    ["node_modules/@types/node/index.d.ts", "node_modules/@types/node/index.d.ts"],
    ["src/../a.ts", "a.ts"],
    ["docs/café.md", "docs/café.md"],
    [`${root}/src/a.ts:7`, "src/a.ts", 7],
    [`${root}/src/a.ts`, "src/a.ts"],
    ["a.ts:0", "a.ts"],
  ] as const)
    assert.deepEqual(codeReference(code, [root]), line ? { path, line } : { path }, code);
  assert.deepEqual(codeReference("C:/proj/app.ts:3", ["C:\\proj"]), { path: "app.ts", line: 3 });
});

test("in a project on a Windows drive, paths written with backslashes count too", () => {
  const windows = ["C:\\Users\\Me\\app"];
  for (const [code, path, line] of [
    ["src\\a.ts", "src/a.ts"],
    ["src\\a.ts:12", "src/a.ts", 12],
    ["C:\\Users\\Me\\app\\src\\a.ts:7", "src/a.ts", 7],
    ["c:\\users\\me\\app\\README.md", "README.md"],
    [".\\main.rs", "main.rs"],
  ] as const)
    assert.deepEqual(codeReference(code, windows), line ? { path, line } : { path }, code);
  for (const code of ["D:\\other\\a.ts", "..\\x.ts", "\\\\server\\share\\a.ts", "src\\", "\\d+\\.ts", "C:\\Users\\Me\\application\\a.ts"])
    assert.equal(codeReference(code, windows), undefined, code);
  // Elsewhere a backslash is an escape, not a folder.
  assert.equal(codeReference("src\\a.ts", [root]), undefined);
});

test("code that is a command, a call, a word, an address or a path outside the project names no file", () => {
  for (const code of [
    "",
    "   ",
    "npm test",
    "foo()",
    "a = b",
    "--flag",
    "-rf",
    "~/notes.md",
    "~",
    "https://example.com/a.ts",
    "file:///Users/me/work/app/a.ts",
    "1.2.3",
    "v2.0",
    "Makefile",
    "README",
    "src/",
    "./",
    ".",
    "..",
    "../x",
    "../x.ts",
    "src/../../x.ts",
    "$HOME/x.ts",
    "@scope/pkg",
    "a,b.ts",
    "<div>",
    "x:y",
    "a.ts:x",
    "C:\\proj\\app.ts",
    "src\\a.ts",
    "*.ts",
    "src/**/a.ts",
    "a.ts#L3",
    "a.ts?raw",
    "50%.md",
    "'a.ts'",
    "`a.ts`",
    "a.ts && b.ts",
    "/etc/passwd",
    "/Users/me/work/application/a.ts",
    "//host/share/a.ts",
    "tel:123",
    "x-acme.settings:privacy",
    "a".repeat(298) + ".ts",
  ])
    assert.equal(codeReference(code, [root]), undefined, code);
  // Nothing is a path of no project.
  assert.equal(codeReference("src/a.ts", []), undefined);
  assert.equal(codeReference("src/a.ts", [""]), undefined);
});

test("a path under any of the project's folders counts, and in a chat without a project only its own folder does", () => {
  assert.deepEqual(codeReference("/private/tmp/app/a.ts:2", ["/tmp/app", "/private/tmp/app"]), { path: "a.ts", line: 2 });
  const chat = ["/data/Chat folders/2026-10-24/trip"];
  assert.deepEqual(codeReference("plan/day1.md:3", chat), { path: "plan/day1.md", line: 3 });
  assert.deepEqual(codeReference("/data/Chat folders/2026-10-24/trip/notes.md", chat), undefined, "a space is never a path in code");
  assert.equal(codeReference("../other/secret.md", chat), undefined);
  assert.deepEqual(fileFolder("a.ts"), { folder: "", name: "a.ts" });
  assert.deepEqual(fileFolder("src/x/a.ts"), { folder: "src/x", name: "a.ts" });
});

/** A project whose folders hold `files`; each listing waits until `release` lets it go, and is counted. */
function project(files: Record<string, string[]>, failing: Record<string, Error> = {}) {
  const listed: string[] = [];
  const held: (() => void)[] = [];
  let running = 0;
  let most = 0;
  const list: FolderLister = (folder) => {
    listed.push(folder);
    running++;
    most = Math.max(most, running);
    return new Promise((done, fail) =>
      held.push(() => {
        running--;
        if (failing[folder]) return fail(failing[folder]);
        if (!(folder in files)) return fail(new Error("Error invoking remote method 'files': This folder no longer exists"));
        done({
          entries: files[folder].map((name) =>
            name.endsWith("/")
              ? { name: name.slice(0, -1), directory: true, symlink: false }
              : name.endsWith("@")
                ? { name: name.slice(0, -1), directory: false, symlink: true }
                : { name, directory: false, symlink: false },
          ),
        });
      }),
    );
  };
  // Each listing starts a moment after it is asked for; let them all answer, and those they lead to.
  const release = async () => {
    for (let round = 0; round < 10; round++) {
      await settle();
      held.splice(0).forEach((go) => go());
    }
    await settle();
  };
  return { list, listed, release, most: () => most, held };
}

test("a file is known once its folder's listing answers, a folder is listed once for all its files, and folders and links do not count", async () => {
  const signal = reply.signal;
  const { list, listed, release } = project({ "": ["README.md", "src/", "linked.md@"], src: ["a.ts", "b.ts"] });
  for (const path of ["src/a.ts", "src/missing.ts", "README.md", "src", "linked.md"])
    assert.equal(codeFileKnown("p", path, list, signal), undefined, path);
  await release();
  assert.deepEqual(listed, ["src", ""]);
  assert.equal(codeFileKnown("p", "src/a.ts", list, signal), true);
  assert.equal(codeFileKnown("p", "src/missing.ts", list, signal), false);
  assert.equal(codeFileKnown("p", "README.md", list, signal), true);
  assert.equal(codeFileKnown("p", "src", list, signal), false, "a folder is not a file to open");
  assert.equal(codeFileKnown("p", "linked.md", list, signal), false, "a link may lead out of the project");
  // A folder that is not there is an answer: nothing in it.
  assert.equal(codeFileKnown("p", "gone/a.ts", list, signal), undefined);
  await release();
  assert.equal(codeFileKnown("p", "gone/a.ts", list, signal), false);
  // Remembered per project: another project's folder of the same name is its own.
  assert.equal(codeFileKnown("q", "src/a.ts", list, signal), undefined);
  await release();
  assert.deepEqual(listed, ["src", "", "gone", "src"]);
});

test("only the names asked about are kept; one asked later has its folder listed again for it", async () => {
  CODE_LINK_LIMITS.again = 0;
  const signal = reply.signal;
  const { list, listed, release } = project({ src: ["a.ts", "b.ts"] });
  codeFileKnown("p", "src/a.ts", list, signal);
  await release();
  assert.equal(codeFileKnown("p", "src/a.ts", list, signal), true);
  // b.ts was listed too, but nothing asked about it, so nothing is known of it.
  assert.equal(codeFileKnown("p", "src/b.ts", list, signal), undefined);
  await release();
  assert.equal(codeFileKnown("p", "src/b.ts", list, signal), true);
  assert.equal(codeFileKnown("p", "src/a.ts", list, signal), true);
  assert.deepEqual(listed, ["src", "src"]);
});

test("while the connection is down nothing is listed and the last answers stand; once it is back, its folders are listed again at once", async () => {
  CODE_LINK_LIMITS.again = 0;
  CODE_LINK_LIMITS.recheck = 40;
  const signal = reply.signal;
  const { list, listed, release } = project({ src: ["a.ts"] });
  codeLinksConnection("p", false);
  assert.equal(codeFileKnown("p", "src/a.ts", list, signal), undefined);
  await release();
  assert.deepEqual(listed, []);
  codeLinksConnection("p", true);
  codeFileKnown("p", "src/a.ts", list, signal);
  await release();
  assert.deepEqual(listed, ["src"]);
  // Down again: a turn's end, a change that waits, and the time passing list nothing.
  codeLinksConnection("p", false);
  projectChanged("p", true);
  projectChanged("p");
  assert.equal(codeFileKnown("p", "src/a.ts", list, signal), true);
  await sleep(60);
  await release();
  assert.deepEqual(listed, ["src"]);
  // Back up: the replies on screen are told, and the folder is listed at once.
  const told: string[] = [];
  const stop = onCodeLinksChanged((scope) => told.push(scope));
  try {
    codeLinksConnection("p", true);
    assert.deepEqual(told, ["p"]);
  } finally {
    stop();
  }
  codeFileKnown("p", "src/a.ts", list, signal);
  await release();
  assert.deepEqual(listed, ["src", "src"]);
});

test("a few listings run at a time, and one whose replies have all gone never starts", async () => {
  const folders = Object.fromEntries(Array.from({ length: 8 }, (_, index) => [`d${index}`, ["a.ts"]]));
  const { list, listed, release, most, held } = project(folders);
  const gone = new AbortController();
  const live = reply.signal;
  for (let index = 0; index < 6; index++) codeFileKnown("p", `d${index}/a.ts`, list, gone.signal);
  codeFileKnown("p", "d6/a.ts", list, live);
  await settle();
  assert.equal(held.length, CODE_LINK_LIMITS.parallel);
  gone.abort();
  await release();
  // Three had started; of the rest only the listing a reply still waits for runs.
  assert.deepEqual(listed, ["d0", "d1", "d2", "d6"]);
  assert.equal(most(), CODE_LINK_LIMITS.parallel);
  assert.equal(codeFileKnown("p", "d6/a.ts", list, live), true);
});

test("a listing that gets no answer keeps the last answer; it is taken again after the recheck time while the connection is up, and not while it is down", async () => {
  CODE_LINK_LIMITS.again = 0;
  CODE_LINK_LIMITS.recheck = 40;
  const signal = reply.signal;
  const failing: Record<string, Error> = {};
  const { list, listed, release } = project({ src: ["a.ts"] }, failing);
  codeFileKnown("p", "src/a.ts", list, signal);
  await release();
  assert.equal(codeFileKnown("p", "src/a.ts", list, signal), true);
  // The window came to the front: listed again, and the server fails.
  failing.src = new Error("Error invoking remote method 'files': fetch failed");
  projectChanged("p");
  await sleep(60);
  assert.equal(codeFileKnown("p", "src/a.ts", list, signal), true);
  await release();
  assert.equal(codeFileKnown("p", "src/a.ts", list, signal), true, "the link stays while nothing is learned");
  assert.equal(listed.length, 2);
  // Tried again once the recheck time is up, while a reply still asks.
  await sleep(60);
  await release();
  assert.equal(listed.length, 3);
  // The connection is down: the retries stop, and the link stays.
  codeLinksConnection("p", false);
  await sleep(60);
  await release();
  await sleep(60);
  await release();
  assert.equal(listed.length, 3);
  assert.equal(codeFileKnown("p", "src/a.ts", list, signal), true);
  // Back up: listed at once.
  delete failing.src;
  codeLinksConnection("p", true);
  codeFileKnown("p", "src/a.ts", list, signal);
  await release();
  assert.equal(listed.length, 4);
});

test("a listing sent before the connection dropped that gives up after it is back is listed again within the least time", async () => {
  CODE_LINK_LIMITS.again = 0;
  CODE_LINK_LIMITS.recheck = 60_000;
  const signal = reply.signal;
  const failing: Record<string, Error> = {};
  const { list, listed, release, held } = project({ src: ["a.ts"] }, failing);
  codeFileKnown("p", "src/a.ts", list, signal);
  await settle();
  assert.equal(held.length, 1);
  // The connection drops and comes back while the listing waits on the old one, which then fails.
  codeLinksConnection("p", false);
  codeLinksConnection("p", true);
  failing.src = new Error("Error invoking remote method 'files': socket hang up");
  held.splice(0).forEach((go) => go());
  await settle();
  delete failing.src;
  await release();
  // Listed again at once, not after the recheck time.
  assert.deepEqual(listed, ["src", "src"]);
  assert.equal(codeFileKnown("p", "src/a.ts", list, signal), true);
});

test("a listing due at once is not held back by a wait for a change that waits", async () => {
  CODE_LINK_LIMITS.again = 100;
  CODE_LINK_LIMITS.recheck = 3_000;
  const signal = reply.signal;
  const { list, listed, release } = project({ src: ["a.ts"] });
  codeFileKnown("p", "src/a.ts", list, signal);
  await release();
  // Another chat's turn ends: listed again once the recheck time is up.
  projectChanged("p");
  codeFileKnown("p", "src/a.ts", list, signal);
  await sleep(20);
  // The viewed chat's turn ends: listed once the least time between listings is up, not at the recheck time.
  projectChanged("p", true);
  codeFileKnown("p", "src/a.ts", list, signal);
  await sleep(200);
  await release();
  assert.deepEqual(listed, ["src", "src"]);
  // And the wait it replaced lists nothing more.
  await sleep(100);
  await release();
  assert.deepEqual(listed, ["src", "src"]);
});

test("the project changing has a folder listed again: at once after a turn or a file write, else after the recheck time", async () => {
  CODE_LINK_LIMITS.again = 0;
  CODE_LINK_LIMITS.recheck = 60_000;
  const signal = reply.signal;
  const files: Record<string, string[]> = { src: ["a.ts"], docs: ["x.md"] };
  const { list, listed, release } = project(files);
  const told: string[] = [];
  const stop = onCodeLinksChanged((scope) => told.push(scope));
  try {
    codeFileKnown("p", "src/new.ts", list, signal);
    codeFileKnown("p", "src/a.ts", list, signal);
    codeFileKnown("p", "docs/x.md", list, signal);
    await release();
    assert.equal(codeFileKnown("p", "src/new.ts", list, signal), false);
    assert.deepEqual(told, ["p", "p"], "each folder's first answer that names files is told");
    told.length = 0;

    // A change that waits (another chat's turn, the window coming to the front): not before the recheck time.
    files.src.push("new.ts");
    projectChanged("p");
    assert.deepEqual(told, ["p"], "replies look again");
    codeFileKnown("p", "src/new.ts", list, signal);
    await release();
    assert.deepEqual(listed, ["src", "docs"]);

    // The chat being viewed ends its turn: at once, and the new file links.
    told.length = 0;
    projectChanged("p", true);
    assert.equal(codeFileKnown("p", "src/new.ts", list, signal), false, "the last answer stands until the listing answers");
    await release();
    assert.equal(codeFileKnown("p", "src/new.ts", list, signal), true);
    assert.deepEqual(told, ["p", "p"]);

    // The file API wrote a file: its folder only is listed again at once.
    listed.length = 0;
    files.docs.push("y.md");
    projectFileWritten("p", "./docs/y.md");
    codeFileKnown("p", "docs/y.md", list, signal);
    codeFileKnown("p", "src/a.ts", list, signal);
    await release();
    assert.deepEqual(listed, ["docs"]);
    assert.equal(codeFileKnown("p", "docs/y.md", list, signal), true);
    // An answer like the last one is not told again.
    told.length = 0;
    projectChanged("p", true);
    told.length = 0;
    codeFileKnown("p", "docs/y.md", list, signal);
    await release();
    assert.deepEqual(told, []);
  } finally {
    stop();
  }
});

test("a folder is listed at most once in the least time between listings, however often it changes", async () => {
  CODE_LINK_LIMITS.again = 300;
  const signal = reply.signal;
  const { list, listed, release } = project({ src: ["a.ts"] });
  codeFileKnown("p", "src/a.ts", list, signal);
  await release();
  for (let index = 0; index < 5; index++) {
    projectChanged("p", true);
    codeFileKnown("p", "src/a.ts", list, signal);
    await release();
  }
  assert.equal(listed.length, 1);
  await sleep(350);
  await release();
  assert.equal(listed.length, 2);
});

test("a listing that runs out of time is not followed at once by another", async () => {
  CODE_LINK_LIMITS.wait = 30;
  CODE_LINK_LIMITS.recheck = 200;
  const signal = reply.signal;
  const { list, listed, release } = project({ src: ["a.ts"] });
  codeFileKnown("p", "src/a.ts", list, signal);
  await sleep(60);
  // The server never answered: the next listing waits the recheck time from when this one gave up.
  assert.deepEqual(listed, ["src"]);
  projectChanged("p");
  codeFileKnown("p", "src/a.ts", list, signal);
  await sleep(60);
  assert.deepEqual(listed, ["src"]);
  await sleep(220);
  assert.deepEqual(listed, ["src", "src"]);
  await release();
});

test("a large folder is listed again at most once in the longer time, whatever changes", async () => {
  CODE_LINK_LIMITS.again = 0;
  CODE_LINK_LIMITS.large = 5;
  CODE_LINK_LIMITS.largeRecheck = 200;
  const signal = reply.signal;
  const { list, listed, release } = project({ logs: Array.from({ length: 8 }, (_, index) => `${index}.log`), src: ["a.ts"] });
  codeFileKnown("p", "logs/1.log", list, signal);
  codeFileKnown("p", "src/a.ts", list, signal);
  await release();
  assert.equal(codeFileKnown("p", "logs/1.log", list, signal), true);
  // A turn ends and another name is asked about: the small folder is listed again, the large one waits.
  projectChanged("p", true);
  codeFileKnown("p", "logs/2.log", list, signal);
  codeFileKnown("p", "src/a.ts", list, signal);
  await release();
  assert.deepEqual(listed, ["logs", "src", "src"]);
  assert.equal(codeFileKnown("p", "logs/2.log", list, signal), undefined);
  await sleep(240);
  await release();
  assert.deepEqual(listed, ["logs", "src", "src", "logs"]);
  assert.equal(codeFileKnown("p", "logs/2.log", list, signal), true);
});

test("a connection back up has its projects' folders listed again at once, after a listing that got no answer too", async () => {
  CODE_LINK_LIMITS.again = 0;
  const signal = reply.signal;
  const failing: Record<string, Error> = {};
  const { list, listed, release } = project({ src: ["a.ts"] }, failing);
  const told: string[] = [];
  const stop = onCodeLinksChanged((scope) => told.push(scope));
  try {
    codeFileKnown("c1\n/chats/trip", "src/a.ts", list, signal);
    codeFileKnown("c2", "src/a.ts", list, signal);
    failing.src = new Error("fetch failed");
    await release();
    assert.equal(listed.length, 2);
    delete failing.src;
    told.length = 0;
    codeLinksConnection("c1", false);
    codeLinksConnection("c1", true);
    // Still up: nothing changes.
    codeLinksConnection("c2", true);
    assert.deepEqual(told, ["c1\n/chats/trip"], "only that connection's projects look again");
    codeFileKnown("c1\n/chats/trip", "src/a.ts", list, signal);
    codeFileKnown("c2", "src/a.ts", list, signal);
    await release();
    assert.equal(listed.length, 3);
    assert.equal(codeFileKnown("c1\n/chats/trip", "src/a.ts", list, signal), true);
    assert.equal(codeFileKnown("c2", "src/a.ts", list, signal), undefined);
  } finally {
    stop();
  }
});

/** Just enough of the DOM for linkCodeSpans, which these tests have no browser for. */
class FakeCode {
  attributes = new Map<string, string>();
  classes = new Set<string>();
  readonly classList = {
    add: (name: string) => void this.classes.add(name),
    remove: (name: string) => void this.classes.delete(name),
    contains: (name: string) => this.classes.has(name),
    get length() {
      return 0;
    },
  };
  constructor(
    public textContent: string,
    readonly inside: string[] = [],
  ) {
    const classes = this.classes;
    Object.defineProperty(this.classList, "length", { get: () => classes.size });
  }
  closest(selector: string) {
    return selector.split(",").some((part) => this.inside.includes(part.trim())) ? {} : null;
  }
  setAttribute(name: string, value: string) {
    this.attributes.set(name, value);
  }
  removeAttribute(name: string) {
    this.attributes.delete(name);
  }
}
const pass = (codes: FakeCode[], known: (path: string) => boolean | undefined, asked = new Set<string>(), live = false) =>
  linkCodeSpans({ querySelectorAll: () => codes } as unknown as ParentNode, { roots: [root], known, asked, live });
const link = (code: FakeCode) => codeLinkTarget(code as unknown as Element);
const shape = (code: FakeCode) => [code.classes.has(CODE_LINK), code.attributes.get("role"), code.attributes.get("tabindex")];

test("code naming a file that is there becomes a link the app knows; the rest stays as the reply wrote it", () => {
  const there = new Set(["src/a.ts", "README.md"]);
  const known = (path: string) => (path === "src/later.ts" ? undefined : there.has(path));
  const file = new FakeCode("src/a.ts:150");
  const top = new FakeCode("README.md");
  const missing = new FakeCode("src/missing.ts");
  const waiting = new FakeCode("src/later.ts");
  const command = new FakeCode("npm test");
  const block = new FakeCode("src/a.ts", ["pre"]);
  const labelled = new FakeCode("src/a.ts", ["a"]);
  const summary = new FakeCode("src/a.ts", ["summary"]);
  pass([file, top, missing, waiting, command, block, labelled, summary], known);
  assert.deepEqual(shape(file), [true, "link", "0"]);
  assert.deepEqual(link(file), { path: "src/a.ts", line: 150 });
  assert.deepEqual(link(top), { path: "README.md" });
  for (const code of [missing, waiting, command, block, labelled, summary]) {
    assert.deepEqual(shape(code), [false, undefined, undefined], code.textContent);
    assert.equal(link(code), undefined);
  }
  // A reply cannot dress its own code as one: the app keeps which code it linked.
  const forged = new FakeCode("src/a.ts");
  forged.classes.add(CODE_LINK);
  assert.equal(link(forged), undefined);
  assert.equal(codeLinkTarget(null), undefined);

  // The reply being written again, or the file gone, makes it plain code again; a link
  // whose listing is not known for now stays.
  pass([file, top], (path) => (path === "src/a.ts" ? undefined : false));
  assert.deepEqual(shape(file), [true, "link", "0"]);
  assert.deepEqual(shape(top), [false, undefined, undefined]);
  assert.equal(top.attributes.has("class"), false);
  pass([file], () => true, new Set(), true);
  assert.deepEqual(shape(file), [false, undefined, undefined]);
  assert.equal(link(file), undefined);
});

test("one reply looks up a limited number of paths; past them code stays as written", () => {
  const asked = new Set<string>();
  const looked: string[] = [];
  const codes = Array.from({ length: CODE_LINK_LIMITS.paths + 5 }, (_, index) => new FakeCode(`src/f${index}.ts`));
  // The same path twice counts once.
  codes.push(new FakeCode("src/f0.ts:3"));
  pass(codes, (path) => (looked.push(path), true), asked);
  assert.equal(asked.size, CODE_LINK_LIMITS.paths);
  assert.equal(codes.filter((code) => link(code)).length, CODE_LINK_LIMITS.paths + 1);
  assert.equal(link(codes.at(-2)!), undefined);
  assert.deepEqual(link(codes.at(-1)!), { path: "src/f0.ts", line: 3 });
  // Drawn again, it looks up the same paths and no others.
  looked.length = 0;
  pass(codes, (path) => (looked.push(path), true), asked);
  assert.equal(new Set(looked).size, CODE_LINK_LIMITS.paths);
});
