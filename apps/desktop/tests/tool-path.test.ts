import test from "node:test";
import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { knownHomeFolders, toolEnvironment, type Files, type FolderCheck } from "../src/main/tool-path";

const ME = 501;
type Folder = { mode?: number; uid?: number; file?: boolean };
/** A file system where only `folders` are there (as folders of ME, 0755,
 * unless they say otherwise), noting every path looked at. */
function only(folders: string[] | Record<string, Folder>): FolderCheck & { asked: string[] } {
  const known: Record<string, Folder> = Array.isArray(folders)
    ? Object.fromEntries(folders.map((folder) => [folder, {}]))
    : folders;
  const asked: string[] = [];
  const check = (path: string) => {
    asked.push(path);
    const folder = known[path];
    return folder && { directory: !folder.file, mode: folder.mode ?? 0o755, uid: folder.uid ?? ME };
  };
  return Object.assign(check, { asked });
}
/** No nvm, nor any of its files. */
const nothing: Files = { list: () => [], read: () => undefined };
const dock = "/usr/bin:/bin:/usr/sbin:/sbin";
const always = ["/Users/me/.bun/bin", "/opt/homebrew/bin", "/usr/local/bin"];
function mac(PATH: string | undefined, folder: FolderCheck, env: Record<string, string> = {}) {
  const result = toolEnvironment({
    platform: "darwin",
    env: { PATH, HOME: "/Users/me", ...env },
    accountHome: "/Users/me",
    uid: ME,
    folder,
    files: nothing,
  });
  return { path: result.PATH.split(":"), tools: result.HAARNESS_TOOL_PATH?.split(":") ?? [] };
}

test("PATH stays as Desktop always made it, and the tool folders that are there are named apart", () => {
  const { path, tools } = mac(
    dock,
    only(["/Users/me/.local/bin", "/Users/me/.cargo/bin", "/opt/homebrew/sbin", "/Users/me/.pyenv/shims"]),
  );
  assert.deepEqual(path, [...dock.split(":"), ...always]);
  assert.deepEqual(tools, ["/Users/me/.local/bin", "/Users/me/.cargo/bin", "/opt/homebrew/sbin", "/Users/me/.pyenv/shims"]);
});

test("a folder that is not there is never named, and on a Mac nothing under /home is looked at", () => {
  const folder = only([]);
  const result = toolEnvironment({
    platform: "darwin",
    env: { PATH: dock, HOME: "/Users/me" },
    accountHome: "/Users/me",
    uid: ME,
    folder,
    files: nothing,
  });
  assert.deepEqual(result, { PATH: [...dock.split(":"), ...always].join(":") });
  assert.deepEqual(folder.asked, [
    // Where nvm is.
    "/Users/me/.nvm",
    "/Users/me/.config/nvm",
    "/Users/me/.local/bin",
    "/Users/me/.cargo/bin",
    "/Users/me/go/bin",
    "/Users/me/.deno/bin",
    "/Users/me/bin",
    "/opt/homebrew/sbin",
    "/usr/local/sbin",
    "/Users/me/.volta/bin",
    "/Users/me/.asdf/shims",
    "/Users/me/.local/share/mise/shims",
    "/Users/me/.pyenv/shims",
    "/Users/me/.rbenv/shims",
    "/Users/me/.nodenv/shims",
    "/Users/me/.local/share/fnm/aliases/default/bin",
    "/Users/me/.fnm/aliases/default/bin",
    "/Users/me/Library/Application Support/fnm/aliases/default/bin",
    "/Users/me/.nvm/current/bin",
    "/Users/me/.local/share/pnpm",
    "/Users/me/Library/pnpm",
    "/Users/me/.yarn/bin",
  ]);
});

test("on Linux Homebrew's Linux folders are looked at, and a Mac's are not", () => {
  const folder = only(["/home/me/.local/bin", "/home/linuxbrew/.linuxbrew/bin", "/home/linuxbrew/.linuxbrew/sbin"]);
  const result = toolEnvironment({
    platform: "linux",
    env: { PATH: "/usr/local/bin:/usr/bin:/bin", HOME: "/home/me" },
    accountHome: "/home/me",
    uid: ME,
    folder,
    files: nothing,
  });
  assert.equal(result.PATH, "/usr/local/bin:/usr/bin:/bin:/home/me/.bun/bin:/opt/homebrew/bin");
  assert.equal(
    result.HAARNESS_TOOL_PATH,
    "/home/me/.local/bin:/home/linuxbrew/.linuxbrew/bin:/home/linuxbrew/.linuxbrew/sbin",
  );
  for (const macOnly of ["/opt/homebrew/sbin", "/usr/local/sbin", "/home/me/Library/pnpm"])
    assert.ok(!folder.asked.includes(macOnly), macOnly);
});

test("the person's own PATH stays first and as it is, and a folder on it is not named again", () => {
  // A terminal's PATH, with a trailing slash, a doubled one, a repeat, a
  // relative folder and an empty entry of the person's own.
  const own = "/Users/me/.local/bin/:/opt/homebrew//bin:.:/usr/bin:/Users/me/.cargo/bin:/usr/bin::/bin";
  const { path, tools } = mac(own, only(["/Users/me/.local/bin", "/Users/me/.cargo/bin", "/Users/me/go/bin"]));
  assert.deepEqual(path, [...own.split(":"), "/Users/me/.bun/bin", "/usr/local/bin"]);
  assert.deepEqual(tools, ["/Users/me/go/bin"]);
});

test("a folder under the home is the person's or the system's and only they may write in it; Homebrew's may be its group's", () => {
  const { tools } = mac(
    dock,
    only({
      "/Users/me/.local/bin": {},
      "/Users/me/.cargo/bin": { uid: 0 },
      "/Users/me/go/bin": { uid: 502 },
      "/Users/me/.deno/bin": { mode: 0o775 },
      "/Users/me/bin": { mode: 0o757 },
      "/Users/me/.volta/bin": { file: true },
      "/opt/homebrew/sbin": { mode: 0o775, uid: 502 },
      "/usr/local/sbin": { mode: 0o777, uid: 0 },
    }),
  );
  assert.deepEqual(tools, ["/Users/me/.local/bin", "/Users/me/.cargo/bin", "/opt/homebrew/sbin"]);
});

test("the home the agent's shell sees decides, and a relative one names nothing from a home folder", () => {
  // A terminal or a test that moved HOME.
  assert.deepEqual(mac(dock, only(["/tmp/elsewhere/.local/bin"]), { HOME: "/tmp/elsewhere" }).tools, [
    "/tmp/elsewhere/.local/bin",
  ]);
  // A HOME that is not absolute is not used; the account's home is.
  assert.deepEqual(mac(dock, only(["/Users/me/.local/bin"]), { HOME: "relative/home" }).tools, ["/Users/me/.local/bin"]);
  const folder = only(["relative/.local/bin"]);
  const result = toolEnvironment({ platform: "darwin", env: { PATH: dock }, accountHome: "relative", uid: ME, folder, files: nothing });
  assert.deepEqual(result, { PATH: [...dock.split(":"), "/opt/homebrew/bin", "/usr/local/bin"].join(":") });
  assert.ok(folder.asked.every((asked) => asked.startsWith("/")));
});

test("without a PATH of its own no empty entry is made, which would search the current folder", () => {
  for (const PATH of [undefined, ""]) {
    const { path } = mac(PATH, only([]));
    assert.deepEqual(path, ["/Users/me/.bun/bin", "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]);
  }
});

test("a packaged app's own programs come first, and are not added again", () => {
  const { PATH } = toolEnvironment({
    platform: "darwin",
    env: { PATH: dock, HOME: "/Users/me" },
    accountHome: "/Users/me",
    bundled: ["/Applications/Haarness.app/Contents/Resources/bin"],
    uid: ME,
    folder: only(["/Applications/Haarness.app/Contents/Resources/bin"]),
    files: nothing,
  });
  assert.equal(PATH.split(":")[0], "/Applications/Haarness.app/Contents/Resources/bin");
  assert.equal(PATH.split(":").filter((entry) => entry.startsWith("/Applications/")).length, 1);
});

test("tool folders the person named in the variable come first", () => {
  const { tools } = mac(dock, only(["/Users/me/.local/bin"]), {
    HAARNESS_TOOL_PATH: "/opt/tools/bin:relative:/Users/me/.local/bin",
  });
  assert.deepEqual(tools, ["/opt/tools/bin", "/Users/me/.local/bin"]);
});

test("fnm's default, pnpm's and Yarn's global folders are named after the shims", () => {
  const { tools } = mac(
    dock,
    only([
      "/Users/me/.asdf/shims",
      "/Users/me/Library/Application Support/fnm/aliases/default/bin",
      "/Users/me/Library/pnpm",
      "/Users/me/.yarn/bin",
    ]),
  );
  assert.deepEqual(tools, [
    "/Users/me/.asdf/shims",
    "/Users/me/Library/Application Support/fnm/aliases/default/bin",
    "/Users/me/Library/pnpm",
    "/Users/me/.yarn/bin",
  ]);
});

test("on Windows only Bun's folder follows the person's PATH, once, and nothing is looked for", () => {
  const folder = only([]);
  const windows = (PATH: string) =>
    toolEnvironment({ platform: "win32", env: { PATH, HOME: "D:\\elsewhere" }, accountHome: "C:\\Users\\Me", folder, files: nothing });
  assert.deepEqual(windows("C:\\Windows\\system32;C:\\Tools"), { PATH: "C:\\Windows\\system32;C:\\Tools;C:\\Users\\Me\\.bun\\bin" });
  // Already there, in another case and with a trailing separator.
  assert.deepEqual(windows("c:\\users\\me\\.BUN\\bin\\;C:\\Tools"), { PATH: "c:\\users\\me\\.BUN\\bin\\;C:\\Tools" });
  assert.deepEqual(folder.asked, []);
});

test("the MCP launcher lookup looks in no folder under the home that this list lacks", async () => {
  // Its own code, without its tests.
  const source = (
    await readFile(new URL("../../../crates/haarness-mcp/src/executable.rs", import.meta.url), "utf8")
  ).split("#[cfg(all(test")[0];
  const folders = [...source.matchAll(/home\.join\("([^"]+)"\)/g)].map((match) => match[1]);
  assert.ok(folders.length >= 10, `found ${folders.length}`);
  for (const folder of folders) assert.ok(knownHomeFolders.includes(folder), folder);
  // Its system folders are always on Desktop's PATH.
  for (const [, folder] of source.matchAll(/PathBuf::from\("(\/[^"]+)"\)/g))
    assert.ok(always.includes(folder) || folder.startsWith("/usr/bin") || folder === "/bin", folder);
});

test("real folders: only one that is there, a folder, the person's, and writable by them alone is named", async (t) => {
  if (process.platform === "win32") return t.skip("Windows folders have no such mode");
  const home = await realpath(await mkdtemp(join(tmpdir(), "haarness-tool-path-")));
  t.after(() => rm(home, { recursive: true, force: true }));
  await mkdir(join(home, ".local/bin"), { recursive: true });
  await mkdir(join(home, "go/bin"), { recursive: true });
  await chmod(join(home, "go/bin"), 0o777);
  await mkdir(join(home, "bin"));
  await chmod(join(home, "bin"), 0o775);
  // Through a link the folder it leads to counts.
  await mkdir(join(home, "open"), { recursive: true });
  await chmod(join(home, "open"), 0o777);
  await mkdir(join(home, ".deno"));
  await symlink(join(home, "open"), join(home, ".deno/bin"));
  await mkdir(join(home, ".cargo"));
  await writeFile(join(home, ".cargo/bin"), "");
  const { HAARNESS_TOOL_PATH } = toolEnvironment({ platform: process.platform, env: { PATH: "/usr/bin:/bin", HOME: home }, accountHome: home });
  const tools = HAARNESS_TOOL_PATH?.split(":") ?? [];
  assert.ok(tools.includes(join(home, ".local/bin")));
  for (const left of ["go/bin", ".deno/bin", ".cargo/bin", "bin", ".volta/bin"]) assert.ok(!tools.includes(join(home, left)), left);
});

/** A home with nvm's folder (`.nvm`, or `root`): these installed versions and these aliases. */
async function nvmHome(t: import("node:test").TestContext, versions: string[], aliases: Record<string, string>, root = ".nvm") {
  const home = await realpath(await mkdtemp(join(tmpdir(), "haarness-tool-path-versions-")));
  t.after(() => rm(home, { recursive: true, force: true }));
  for (const version of versions) await mkdir(join(home, root, "versions/node", version, "bin"), { recursive: true });
  for (const [name, value] of Object.entries(aliases)) {
    await mkdir(join(home, root, "alias", name, ".."), { recursive: true });
    await writeFile(join(home, root, "alias", name), value + "\n");
  }
  return home;
}
/** The tool folders under `home` a Dock-opened Desktop names for it. */
function named(home: string, env: { PATH?: string; NVM_DIR?: string; XDG_CONFIG_HOME?: string } = {}) {
  const result = toolEnvironment({ platform: "darwin", env: { PATH: dock, HOME: home, ...env }, accountHome: home });
  return (result.HAARNESS_TOOL_PATH?.split(":") ?? [])
    .filter((entry) => entry.startsWith(home + "/"))
    .map((entry) => entry.slice(home.length + 1));
}

test("nvm's default version is named last, as nvm would pick it", async (t) => {
  if (process.platform === "win32") return t.skip("nvm for Windows keeps its own PATH");
  const versions = ["v16.20.2", "v18.19.0", "v18.20.4", "v20.11.0", "v9.11.2", "v10.24.1"];
  // A major version picks its newest install, by number.
  const home = await nvmHome(t, versions, { default: "18" });
  await mkdir(join(home, ".yarn/bin"), { recursive: true });
  assert.deepEqual(named(home), [".nvm/versions/node/v18.20.4/bin", ".yarn/bin"]);
  assert.deepEqual(named(await nvmHome(t, versions, { default: "v20.11" })), [".nvm/versions/node/v20.11.0/bin"]);
  assert.deepEqual(named(await nvmHome(t, versions, { default: "v18.19.0" })), [".nvm/versions/node/v18.19.0/bin"]);
  // Aliases lead to it: lts/* → lts/iron → a version.
  assert.deepEqual(
    named(await nvmHome(t, ["v20.11.0", "v22.1.0"], { default: "lts/*", "lts/*": "lts/iron", "lts/iron": "v20.11.0" })),
    [".nvm/versions/node/v20.11.0/bin"],
  );
  // `node`, an alias naming nothing installed, or none: the newest, v20
  // and not v9 as text would sort them.
  for (const aliases of [{ default: "node" }, { default: "14" }, {}] as Record<string, string>[])
    assert.deepEqual(named(await nvmHome(t, versions, aliases)), [".nvm/versions/node/v20.11.0/bin"]);
  // `system` asks for no nvm version.
  assert.deepEqual(named(await nvmHome(t, versions, { default: "system" })), []);
});

test("nvm lives in NVM_DIR, else the first of the XDG config folder, ~/.nvm and ~/.config/nvm", async (t) => {
  if (process.platform === "win32") return t.skip("nvm for Windows keeps its own PATH");
  const config = await nvmHome(t, ["v20.11.0"], {}, ".config/nvm");
  assert.deepEqual(named(config), [".config/nvm/versions/node/v20.11.0/bin"]);
  // Both: ~/.nvm, unless XDG_CONFIG_HOME says otherwise.
  const both = await nvmHome(t, ["v20.11.0"], {}, ".config/nvm");
  await mkdir(join(both, ".nvm/versions/node/v18.20.4/bin"), { recursive: true });
  assert.deepEqual(named(both), [".nvm/versions/node/v18.20.4/bin"]);
  assert.deepEqual(named(both, { XDG_CONFIG_HOME: join(both, ".config") }), [".config/nvm/versions/node/v20.11.0/bin"]);
  // An absolute NVM_DIR wins; a relative one is not used.
  assert.deepEqual(named(both, { NVM_DIR: join(both, ".config/nvm") }), [".config/nvm/versions/node/v20.11.0/bin"]);
  assert.deepEqual(named(both, { NVM_DIR: ".config/nvm" }), [".nvm/versions/node/v18.20.4/bin"]);
});

test("nvm's current link counts, and a version on PATH already or open to others is not named", async (t) => {
  if (process.platform === "win32") return t.skip("nvm for Windows keeps its own PATH");
  const home = await nvmHome(t, ["v20.11.0"], {});
  await symlink(join(home, ".nvm/versions/node/v20.11.0"), join(home, ".nvm/current"));
  assert.deepEqual(named(home), [".nvm/current/bin", ".nvm/versions/node/v20.11.0/bin"]);
  const own = join(home, ".nvm/versions/node/v20.11.0/bin");
  assert.deepEqual(named(home, { PATH: `${own}:${dock}` }), [".nvm/current/bin"]);
  const open = await nvmHome(t, ["v20.11.0"], { default: "20" });
  await chmod(join(open, ".nvm/versions/node/v20.11.0/bin"), 0o777);
  assert.deepEqual(named(open), []);
});

test("nvm's aliases are names in its alias folder, followed a few steps, and only small files are read", async (t) => {
  if (process.platform === "win32") return t.skip("nvm for Windows keeps its own PATH");
  const versions = ["v18.20.4", "v20.11.0"];
  // A path out of the alias folder is not followed: the newest instead.
  const out = await nvmHome(t, versions, { default: "../secret" });
  await writeFile(join(out, ".nvm/secret"), "v18.20.4\n");
  assert.deepEqual(named(out), [".nvm/versions/node/v20.11.0/bin"]);
  // Aliases that name each other end.
  assert.deepEqual(named(await nvmHome(t, versions, { default: "a", a: "b", b: "a" })), [".nvm/versions/node/v20.11.0/bin"]);
  // Only four steps are followed past `default`.
  const chain = { default: "one", one: "two", two: "three", three: "four", four: "18", "18": "v20.11.0" };
  assert.deepEqual(named(await nvmHome(t, versions, chain)), [".nvm/versions/node/v18.20.4/bin"]);
  // A pipe named `default` is never opened, so Desktop does not wait on it.
  const pipe = await nvmHome(t, versions, {});
  await mkdir(join(pipe, ".nvm/alias"), { recursive: true });
  execFileSync("mkfifo", [join(pipe, ".nvm/alias/default")]);
  assert.deepEqual(named(pipe), [".nvm/versions/node/v20.11.0/bin"]);
});
