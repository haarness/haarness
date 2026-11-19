import test from "node:test";
import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { toolPath, type FolderCheck } from "../src/main/tool-path";

/** A file system where only `folders` are there; `open` ones anyone may write to. */
function only(folders: string[], open: string[] = []): FolderCheck & { asked: string[] } {
  const asked: string[] = [];
  const check = (path: string) => {
    asked.push(path);
    if (open.includes(path)) return { directory: true, openToAll: true };
    return folders.includes(path) ? { directory: true, openToAll: false } : undefined;
  };
  return Object.assign(check, { asked });
}
const dock = "/usr/bin:/bin:/usr/sbin:/sbin";
const mac = (PATH: string | undefined, folder: FolderCheck, HOME = "/Users/me") =>
  toolPath({ platform: "darwin", env: { PATH, HOME }, accountHome: "/Users/me", folder }).split(":");

test("Desktop opened from the Dock adds the person's tool folders that are there, after the PATH it got", () => {
  const path = mac(
    dock,
    only(["/Users/me/.local/bin", "/Users/me/.cargo/bin", "/opt/homebrew/sbin", "/Users/me/.pyenv/shims"]),
  );
  assert.deepEqual(path, [
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    // Always there before, and kept.
    "/Users/me/.bun/bin",
    "/opt/homebrew/bin",
    "/usr/local/bin",
    // New, each one found.
    "/Users/me/.local/bin",
    "/Users/me/.cargo/bin",
    "/opt/homebrew/sbin",
    "/Users/me/.pyenv/shims",
  ]);
});

test("a folder that is not there is never added, and every place Haarness knows is only looked at", () => {
  const folder = only([]);
  const path = mac(dock, folder);
  assert.deepEqual(path, ["/usr/bin", "/bin", "/usr/sbin", "/sbin", "/Users/me/.bun/bin", "/opt/homebrew/bin", "/usr/local/bin"]);
  assert.deepEqual(folder.asked, [
    "/Users/me/.local/bin",
    "/Users/me/.cargo/bin",
    "/Users/me/go/bin",
    "/Users/me/.deno/bin",
    "/Users/me/bin",
    "/opt/homebrew/sbin",
    "/usr/local/sbin",
    "/home/linuxbrew/.linuxbrew/bin",
    "/home/linuxbrew/.linuxbrew/sbin",
    "/Users/me/.volta/bin",
    "/Users/me/.asdf/shims",
    "/Users/me/.local/share/mise/shims",
    "/Users/me/.pyenv/shims",
    "/Users/me/.rbenv/shims",
    "/Users/me/.nodenv/shims",
  ]);
});

test("the person's own PATH stays first and as it is, and nothing on it is added twice", () => {
  // A terminal's PATH, with a trailing slash, a doubled one, a repeat, a
  // relative folder and an empty entry of the person's own.
  const own = "/Users/me/.local/bin/:/opt/homebrew//bin:.:/usr/bin:/Users/me/.cargo/bin:/usr/bin::/bin";
  const path = mac(own, only(["/Users/me/.local/bin", "/Users/me/.cargo/bin", "/Users/me/go/bin"]));
  assert.deepEqual(path, [
    ...own.split(":"),
    "/Users/me/.bun/bin",
    "/usr/local/bin",
    "/Users/me/go/bin",
  ]);
  assert.equal(new Set(path.slice(own.split(":").length)).size, path.length - own.split(":").length);
});

test("a folder anyone may write to, or a file, is never added", () => {
  const files: FolderCheck = (path) =>
    path === "/Users/me/.cargo/bin" ? { directory: false, openToAll: false } : only(["/Users/me/.local/bin"], ["/Users/me/go/bin", "/usr/local/sbin"])(path);
  const path = mac(dock, files);
  assert.ok(path.includes("/Users/me/.local/bin"));
  for (const left of ["/Users/me/.cargo/bin", "/Users/me/go/bin", "/usr/local/sbin"]) assert.ok(!path.includes(left), left);
});

test("the home the agent's shell sees decides, and a relative one adds nothing from a home folder", () => {
  // A terminal or a test that moved HOME.
  assert.ok(mac(dock, only(["/tmp/elsewhere/.local/bin"]), "/tmp/elsewhere").includes("/tmp/elsewhere/.local/bin"));
  // A HOME that is not absolute is not used; the account's home is.
  assert.ok(mac(dock, only(["/Users/me/.local/bin"]), "relative/home").includes("/Users/me/.local/bin"));
  const folder = only(["relative/.local/bin"]);
  const path = toolPath({ platform: "darwin", env: { PATH: dock }, accountHome: "relative", folder }).split(":");
  assert.deepEqual(path, ["/usr/bin", "/bin", "/usr/sbin", "/sbin", "/opt/homebrew/bin", "/usr/local/bin"]);
  assert.ok(folder.asked.every((asked) => asked.startsWith("/")));
});

test("without a PATH of its own no empty entry is made, which would search the current folder", () => {
  for (const PATH of [undefined, ""]) {
    const path = mac(PATH, only([]));
    assert.ok(!path.includes(""), JSON.stringify(path));
    assert.deepEqual(path, ["/Users/me/.bun/bin", "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]);
  }
});

test("a packaged app's own programs come first, and are not added again", () => {
  const path = toolPath({
    platform: "darwin",
    env: { PATH: dock, HOME: "/Users/me" },
    accountHome: "/Users/me",
    bundled: ["/Applications/Haarness.app/Contents/Resources/bin"],
    folder: only(["/Applications/Haarness.app/Contents/Resources/bin"]),
  }).split(":");
  assert.equal(path[0], "/Applications/Haarness.app/Contents/Resources/bin");
  assert.equal(path.filter((entry) => entry.startsWith("/Applications/")).length, 1);
});

test("on Linux the same folders are added, with Homebrew's Linux home", () => {
  const path = toolPath({
    platform: "linux",
    env: { PATH: "/usr/local/bin:/usr/bin:/bin", HOME: "/home/me" },
    accountHome: "/home/me",
    folder: only(["/home/me/.local/bin", "/home/linuxbrew/.linuxbrew/bin", "/home/linuxbrew/.linuxbrew/sbin"]),
  }).split(":");
  assert.deepEqual(path, [
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/home/me/.bun/bin",
    "/opt/homebrew/bin",
    "/home/me/.local/bin",
    "/home/linuxbrew/.linuxbrew/bin",
    "/home/linuxbrew/.linuxbrew/sbin",
  ]);
});

test("on Windows only Bun's folder follows the person's PATH, once, and nothing is looked for", () => {
  const folder = only([]);
  const windows = (PATH: string) =>
    toolPath({ platform: "win32", env: { PATH, HOME: "D:\\elsewhere" }, accountHome: "C:\\Users\\Me", folder });
  assert.equal(windows("C:\\Windows\\system32;C:\\Tools"), "C:\\Windows\\system32;C:\\Tools;C:\\Users\\Me\\.bun\\bin");
  // Already there, in another case and with a trailing separator.
  assert.equal(windows("c:\\users\\me\\.BUN\\bin\\;C:\\Tools"), "c:\\users\\me\\.BUN\\bin\\;C:\\Tools");
  assert.deepEqual(folder.asked, []);
});

test("real folders: only a folder that is there, is a folder and is not open to everyone is added", async (t) => {
  if (process.platform === "win32") return t.skip("Windows folders have no such mode");
  const home = await realpath(await mkdtemp(join(tmpdir(), "haarness-tool-path-")));
  t.after(() => rm(home, { recursive: true, force: true }));
  await mkdir(join(home, ".local/bin"), { recursive: true });
  await mkdir(join(home, "go/bin"), { recursive: true });
  await chmod(join(home, "go/bin"), 0o777);
  // Through a link the folder it leads to counts.
  await mkdir(join(home, "open"), { recursive: true });
  await chmod(join(home, "open"), 0o777);
  await mkdir(join(home, ".deno"));
  await symlink(join(home, "open"), join(home, ".deno/bin"));
  await mkdir(join(home, ".cargo"));
  await writeFile(join(home, ".cargo/bin"), "");
  const path = toolPath({ platform: process.platform, env: { PATH: "/usr/bin:/bin", HOME: home }, accountHome: home }).split(":");
  assert.ok(path.includes(join(home, ".local/bin")));
  for (const left of ["go/bin", ".deno/bin", ".cargo/bin", "bin", ".volta/bin"]) assert.ok(!path.includes(join(home, left)), left);
});
