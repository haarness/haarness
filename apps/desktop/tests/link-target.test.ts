import test from "node:test";
import assert from "node:assert/strict";
import { linkTarget, type LinkTarget } from "../src/renderer/link-target";

const project = { root: "/Users/me/work/app" };
const file = (path: string, line?: number): LinkTarget => ({
  kind: "file",
  path,
  ...(line ? { line } : {}),
});
const refused = (reason: "network" | "outside" | "unsupported"): LinkTarget => ({
  kind: "refused",
  reason,
});

test("links to project files open in the Files panel, at their line", () => {
  for (const [href, target] of [
    ["src/a.ts", file("src/a.ts")],
    ["./src/a.ts", file("src/a.ts")],
    ["src/a.ts:12", file("src/a.ts", 12)],
    ["src/a.ts:12:4", file("src/a.ts", 12)],
    ["src/a.ts:12-20", file("src/a.ts", 12)],
    ["src/a.ts#L7", file("src/a.ts", 7)],
    ["src/a.ts#L7-L9", file("src/a.ts", 7)],
    ["src/a.ts?plain=1#L3", file("src/a.ts", 3)],
    ["/Users/me/work/app/src/a.ts:5", file("src/a.ts", 5)],
    ["file:///Users/me/work/app/src/a.ts", file("src/a.ts")],
    ["file://localhost/Users/me/work/app/src/a.ts#L2", file("src/a.ts", 2)],
    ["  src/a.ts\n", file("src/a.ts")],
    // Markdown writes spaces and accents percent-encoded.
    ["docs/Read%20me%20caf%C3%A9.md", file("docs/Read me café.md")],
    ["docs/100%.md", file("docs/100%.md")],
    ["src\\win\\a.ts", file("src/win/a.ts")],
    ["src/", file("src")],
    ["/Users/me/work/app", file("")],
  ] as const)
    assert.deepEqual(linkTarget(href, project), target, href);
});

test("a file that could run or pick its own app still only opens in the Files panel", () => {
  for (const name of ["run.command", "run.sh", "run.terminal", "x.webloc", "Tool.app", "diagram.svg", "page.html"])
    assert.deepEqual(linkTarget(name, project), file(name), name);
});

test("relative links in a Markdown file start from its folder", () => {
  assert.deepEqual(linkTarget("guide.md", { ...project, base: "docs" }), file("docs/guide.md"));
  assert.deepEqual(linkTarget("../README.md", { ...project, base: "docs" }), file("README.md"));
  assert.deepEqual(
    linkTarget("guide.md", { ...project, base: "/Users/me/work/app/docs" }),
    file("docs/guide.md"),
  );
  assert.deepEqual(linkTarget("guide.md", { ...project, base: "/Users/me/other" }), refused("outside"));
  assert.deepEqual(linkTarget("/Users/me/work/app/x.md", { ...project, base: "docs" }), file("x.md"));
});

test("paths outside the project are refused", () => {
  for (const href of [
    "file:///Applications/Calculator.app",
    "/Applications/Calculator.app",
    "/Users/me/work/application/a.ts",
    "../secret.txt",
    "src/../../secret.txt",
    "~/.ssh/id_ed25519",
    "C:\\Windows\\System32\\calc.exe",
    "file:///C:/Windows/System32/calc.exe",
  ])
    assert.deepEqual(linkTarget(href, project), refused("outside"), href);
});

test("network locations are refused before anything resolves them", () => {
  for (const href of [
    "//server/share/x.md",
    "\\\\server\\share\\x.md",
    "/\\server/share",
    "file://server/share/x.md",
    "file:////server/share/x.md",
    "file:///%2F%2Fserver/share",
    "%5C%5Cserver%5Cshare",
    "smb://server/share",
    "AFP://server/share",
    "nfs://server/export",
  ])
    assert.deepEqual(linkTarget(href, project), refused("network"), href);
});

test("other schemes are refused; web and mail links go on to the desktop's own check", () => {
  for (const href of ["javascript:alert(1)", "data:text/html,x", "vscode://file/a.ts", "ftp://host/x", "tel:123"])
    assert.deepEqual(linkTarget(href, project), refused("unsupported"), href);
  for (const href of ["https://example.test/a", "HTTP://example.test", "mailto:me@example.test"])
    assert.deepEqual(linkTarget(href, project), { kind: "web", url: href });
  for (const href of ["", "   ", "#section", "#L4"])
    assert.deepEqual(linkTarget(href, project), { kind: "none" });
});

test("Windows projects compare paths without case", () => {
  const windows = { root: "C:\\Users\\Me\\app" };
  assert.deepEqual(linkTarget("c:\\users\\me\\app\\src\\a.ts:3", windows), file("src/a.ts", 3));
  assert.deepEqual(linkTarget("file:///C:/Users/Me/app/a.ts", windows), file("a.ts"));
  assert.deepEqual(linkTarget("D:\\app\\a.ts", windows), refused("outside"));
});

test("in a chat without a project, links start from the chat's own folder and never leave it", () => {
  const home = { root: "/data/Chat folders", within: "2026-10-24/trip" };
  for (const [href, target] of [
    ["notes.md", file("2026-10-24/trip/notes.md")],
    ["./plan/day1.md:3", file("2026-10-24/trip/plan/day1.md", 3)],
    ["/data/Chat folders/2026-10-24/trip/notes.md", file("2026-10-24/trip/notes.md")],
    ["file:///data/Chat%20folders/2026-10-24/trip/a.png", file("2026-10-24/trip/a.png")],
    // Another chat's folder, the day above and the chat home are outside.
    ["../other/secret.md", refused("outside")],
    ["/data/Chat folders/2026-10-24/other/secret.md", refused("outside")],
    ["..", refused("outside")],
    ["/data/Chat folders/notes.md", refused("outside")],
    ["/etc/hosts", refused("outside")],
  ] as const)
    assert.deepEqual(linkTarget(href, home), target, href);
  // A file preview's own folder still sets where relative links start.
  assert.deepEqual(
    linkTarget("b.md", { ...home, base: "2026-10-24/trip/docs" }),
    file("2026-10-24/trip/docs/b.md"),
  );
});
