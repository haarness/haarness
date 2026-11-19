import test from "node:test";
import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { toolPath, type Files, type FolderCheck } from "../src/main/tool-path";

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
/** No nvm, nor any of its files. */
const nothing: Files = { list: () => [], read: () => undefined };
const dock = "/usr/bin:/bin:/usr/sbin:/sbin";
const mac = (PATH: string | undefined, folder: FolderCheck, HOME = "/Users/me") =>
  toolPath({ platform: "darwin", env: { PATH, HOME }, accountHome: "/Users/me", folder, files: nothing }).split(":");

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
    "/Users/me/.nvm/current/bin",
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
  const path = toolPath({ platform: "darwin", env: { PATH: dock }, accountHome: "relative", folder, files: nothing }).split(":");
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
    files: nothing,
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
    files: nothing,
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
    toolPath({ platform: "win32", env: { PATH, HOME: "D:\\elsewhere" }, accountHome: "C:\\Users\\Me", folder, files: nothing });
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

/** A home with nvm's folder: these installed versions and these aliases. */
async function nvmHome(t: import("node:test").TestContext, versions: string[], aliases: Record<string, string>) {
  const home = await realpath(await mkdtemp(join(tmpdir(), "haarness-tool-path-versions-")));
  t.after(() => rm(home, { recursive: true, force: true }));
  for (const version of versions) await mkdir(join(home, ".nvm/versions/node", version, "bin"), { recursive: true });
  for (const [name, value] of Object.entries(aliases)) {
    await mkdir(join(home, ".nvm/alias", name, ".."), { recursive: true });
    await writeFile(join(home, ".nvm/alias", name), value + "\n");
  }
  return home;
}
/** The nvm folders on the PATH a Dock-opened Desktop gives this home. */
function nvmOnPath(home: string, env: { PATH?: string; NVM_DIR?: string } = {}) {
  return toolPath({ platform: "darwin", env: { PATH: dock, HOME: home, ...env }, accountHome: home })
    .split(":")
    .filter((entry) => entry.startsWith(join(home, ".nvm") + "/"))
    .map((entry) => entry.slice(home.length + 1));
}

test("nvm's default version is added last, as a terminal would pick it", async (t) => {
  if (process.platform === "win32") return t.skip("nvm for Windows keeps its own PATH");
  const versions = ["v16.20.2", "v18.19.0", "v18.20.4", "v20.11.0", "v9.11.2", "v10.24.1"];
  // A major version picks its newest install, by number.
  const home = await nvmHome(t, versions, { default: "18" });
  const path = toolPath({ platform: "darwin", env: { PATH: dock, HOME: home }, accountHome: home }).split(":");
  assert.equal(path.at(-1), join(home, ".nvm/versions/node/v18.20.4/bin"));
  assert.deepEqual(nvmOnPath(await nvmHome(t, versions, { default: "v20.11" })), [".nvm/versions/node/v20.11.0/bin"]);
  assert.deepEqual(nvmOnPath(await nvmHome(t, versions, { default: "v18.19.0" })), [".nvm/versions/node/v18.19.0/bin"]);
  // Aliases lead to it: lts/* → lts/iron → a version.
  assert.deepEqual(
    nvmOnPath(await nvmHome(t, ["v20.11.0", "v22.1.0"], { default: "lts/*", "lts/*": "lts/iron", "lts/iron": "v20.11.0" })),
    [".nvm/versions/node/v20.11.0/bin"],
  );
  // `node`, an alias naming nothing installed, or none: the newest, v20
  // and not v9 as text would sort them.
  for (const aliases of [{ default: "node" }, { default: "14" }, {}] as Record<string, string>[])
    assert.deepEqual(nvmOnPath(await nvmHome(t, versions, aliases)), [".nvm/versions/node/v20.11.0/bin"]);
  // `system` asks for no nvm version.
  assert.deepEqual(nvmOnPath(await nvmHome(t, versions, { default: "system" })), []);
});

test("nvm's current link counts, NVM_DIR moves its folder, and the usual rules hold", async (t) => {
  if (process.platform === "win32") return t.skip("nvm for Windows keeps its own PATH");
  const home = await nvmHome(t, ["v20.11.0"], {});
  await symlink(join(home, ".nvm/versions/node/v20.11.0"), join(home, ".nvm/current"));
  assert.deepEqual(nvmOnPath(home), [".nvm/current/bin", ".nvm/versions/node/v20.11.0/bin"]);
  // Already on the PATH: not again.
  const own = join(home, ".nvm/versions/node/v20.11.0/bin");
  assert.deepEqual(nvmOnPath(home, { PATH: `${own}:${dock}` }), [".nvm/versions/node/v20.11.0/bin", ".nvm/current/bin"]);
  // An absolute NVM_DIR is where nvm lives; a relative one is not used.
  const moved = await nvmHome(t, ["v18.20.4"], {});
  assert.deepEqual(nvmOnPath(home, { NVM_DIR: join(moved, ".nvm") }).length, 0);
  assert.ok(
    toolPath({ platform: "darwin", env: { PATH: dock, HOME: home, NVM_DIR: join(moved, ".nvm") }, accountHome: home })
      .split(":")
      .includes(join(moved, ".nvm/versions/node/v18.20.4/bin")),
  );
  assert.deepEqual(nvmOnPath(home, { NVM_DIR: "relative/.nvm" }), [".nvm/current/bin", ".nvm/versions/node/v20.11.0/bin"]);
  // A version's folder anyone may write to is never added.
  const open = await nvmHome(t, ["v20.11.0"], { default: "20" });
  await chmod(join(open, ".nvm/versions/node/v20.11.0/bin"), 0o777);
  assert.deepEqual(nvmOnPath(open), []);
});

test("nvm's aliases are names in its alias folder, followed a few steps, and only small files are read", async (t) => {
  if (process.platform === "win32") return t.skip("nvm for Windows keeps its own PATH");
  const versions = ["v18.20.4", "v20.11.0"];
  // A path out of the alias folder is not followed: the newest instead.
  const out = await nvmHome(t, versions, { default: "../secret" });
  await writeFile(join(out, ".nvm/secret"), "v18.20.4\n");
  assert.deepEqual(nvmOnPath(out), [".nvm/versions/node/v20.11.0/bin"]);
  // Aliases that name each other end.
  assert.deepEqual(nvmOnPath(await nvmHome(t, versions, { default: "a", a: "b", b: "a" })), [".nvm/versions/node/v20.11.0/bin"]);
  // Only four steps are followed past `default`.
  const chain = { default: "one", one: "two", two: "three", three: "four", four: "18", "18": "v20.11.0" };
  assert.deepEqual(nvmOnPath(await nvmHome(t, versions, chain)), [".nvm/versions/node/v18.20.4/bin"]);
  // A pipe named `default` is never opened, so Desktop does not wait on it.
  const pipe = await nvmHome(t, versions, {});
  await mkdir(join(pipe, ".nvm/alias"), { recursive: true });
  execFileSync("mkfifo", [join(pipe, ".nvm/alias/default")]);
  assert.deepEqual(nvmOnPath(pipe), [".nvm/versions/node/v20.11.0/bin"]);
});
