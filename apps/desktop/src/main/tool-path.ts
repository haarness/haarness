import { opendirSync, readFileSync, statSync } from "node:fs";
import { posix, win32 } from "node:path";

/** What PATH needs to know of a folder; nothing when it is not there. */
export type FolderCheck = (path: string) => { directory: boolean; mode: number; uid: number } | undefined;

/** The little reading nvm's folder needs. */
export interface Files {
  /** Up to `most` names in a folder; none when it cannot be read. */
  list(path: string, most: number): string[];
  /** A small file's text; nothing for anything else. */
  read(path: string): string | undefined;
}

function inspect(path: string) {
  try {
    const stat = statSync(path);
    return { directory: stat.isDirectory(), mode: stat.mode, uid: stat.uid };
  } catch {
    return undefined;
  }
}

const disk: Files = {
  list(path, most) {
    const names: string[] = [];
    try {
      const folder = opendirSync(path);
      try {
        for (let entry = folder.readSync(); entry && names.length < most; entry = folder.readSync())
          names.push(entry.name);
      } finally {
        folder.closeSync();
      }
    } catch {}
    return names;
  },
  read(path) {
    try {
      // A pipe or a device would never finish reading, and Desktop waits.
      const stat = statSync(path);
      return stat.isFile() && stat.size <= 4096 ? readFileSync(path, "utf8") : undefined;
    } catch {
      return undefined;
    }
  },
};

/** Where people's own tools live under the home folder, in the order they
 * are looked in; `macOS` ones only there. The MCP launcher lookup
 * (`crates/haarness-mcp/src/executable.rs`) looks in the same folders: a
 * unit test checks that each of its folders is here. */
const PERSONAL = [".local/bin", ".cargo/bin", "go/bin", ".deno/bin", "bin"];
const MANAGERS = [".volta/bin", ".asdf/shims", ".local/share/mise/shims", ".pyenv/shims", ".rbenv/shims", ".nodenv/shims"];
const FNM = [".local/share/fnm", ".fnm", { macOS: "Library/Application Support/fnm" }];
const NVM = [".nvm", ".config/nvm"];
const AFTER_NVM = [".local/share/pnpm", { macOS: "Library/pnpm" }, ".yarn/bin"];

/** Every folder under the home this module knows, for that test. */
export const knownHomeFolders = [".bun/bin", ...PERSONAL, ...MANAGERS, ...FNM, ...NVM, ...AFTER_NVM].map((folder) =>
  typeof folder === "string" ? folder : folder.macOS,
);

/** `20.18.1`, or a part of one such as `20`, as numbers. */
function versionNumbers(text: string) {
  const parts = text.split(".");
  return parts.every((part) => /^\d+$/.test(part)) ? parts.map(Number) : undefined;
}

/**
 * The bin folders nvm's own files point at, read without running anything:
 * its `current` link (made when `NVM_SYMLINK_CURRENT` is on), then the
 * installed version that the `default` alias resolves to (`18`, `v20.11`, an
 * exact version, or an alias such as `lts/*` that leads to one of those, a
 * few aliases deep at most). `node`, an alias naming nothing installed, or no
 * alias at all give the newest installed version; `system` gives none.
 */
function nvmFolders(path: typeof posix, root: string, files: Files) {
  // Enough for every version anyone keeps; a folder past it is not read.
  const MAX_VERSIONS = 512;
  const folders = [path.join(root, "current", "bin")];
  const installed = path.join(root, "versions", "node");
  const versions = files
    .list(installed, MAX_VERSIONS)
    .flatMap((name) => {
      const numbers = name.startsWith("v") ? versionNumbers(name.slice(1)) : undefined;
      return numbers ? [{ name, numbers }] : [];
    })
    .sort((left, right) => {
      for (let index = 0; index < Math.max(left.numbers.length, right.numbers.length); index++) {
        const difference = (right.numbers[index] ?? -1) - (left.numbers[index] ?? -1);
        if (difference) return difference;
      }
      return 0;
    });
  // An alias is a name inside the alias folder, never a path out of it.
  const alias = (name: string) => {
    if (!name || name.split("/").some((part) => !part || part === "." || part === "..")) return undefined;
    return files.read(path.join(root, "alias", ...name.split("/")))?.trim() || undefined;
  };
  let wanted = alias("default");
  for (let step = 0; wanted && step < 4; step++) {
    const next = alias(wanted);
    if (!next) break;
    wanted = next;
  }
  if (wanted === "system") return folders;
  const numbers = wanted ? versionNumbers(wanted.replace(/^v/, "")) : undefined;
  const picked =
    (numbers && versions.find((version) => numbers.every((number, index) => version.numbers[index] === number))) ||
    versions[0];
  if (picked) folders.push(path.join(installed, picked.name, "bin"));
  return folders;
}

/**
 * What Desktop starts Haarness with. An app opened from the Dock or the
 * Finder gets only the system's folders on PATH (a desktop launcher on Linux,
 * the session's), and the agent's commands run under `sh -l`, which reads
 * `~/.profile` but never a zsh or bash profile: the tools a terminal finds
 * through those (uv, ruff, rust-analyzer, gopls, an nvm node) are missing.
 *
 * `PATH` is what it always was: the app's own programs, the PATH Desktop got,
 * as it is, and the folders Desktop has always added. The usual homes of
 * tools people install themselves go in `HAARNESS_TOOL_PATH` instead, for
 * the server to add to each command's PATH after its login profile ran: a
 * profile that puts `~/.cargo/bin` first only while it is missing (rustup's
 * and uv's setup files do) must still find it missing. Each one is there,
 * not on `PATH` already, and nobody else may write in it: one under the home
 * folder belongs to the person (or the system) and is writable by them alone,
 * and Homebrew's, which its installer leaves writable by its group, is not
 * writable by everyone. Nothing is run to find them. Windows apps get the
 * person's PATH from the registry, so there only Bun's folder follows it, as
 * before, and no tool folders are named.
 */
export function toolEnvironment(options: {
  platform: NodeJS.Platform;
  /** Desktop's own environment. */
  env: { PATH?: string; HOME?: string; NVM_DIR?: string; XDG_CONFIG_HOME?: string; HAARNESS_TOOL_PATH?: string };
  /** The account's home folder, for an environment without a usable `HOME`. */
  accountHome: string;
  /** Folders ahead of everything (a packaged app's own programs). */
  bundled?: string[];
  /** The person's user id; Desktop's own by default. */
  uid?: number;
  folder?: FolderCheck;
  files?: Files;
}): { PATH: string; HAARNESS_TOOL_PATH?: string } {
  const windows = options.platform === "win32";
  const macOS = options.platform === "darwin";
  const path = windows ? win32 : posix;
  const folder = options.folder ?? inspect;
  const absolute = (value: string | undefined) => (value && path.isAbsolute(value) ? value : undefined);
  // The home the agent's shell sees, which a test or a terminal may move.
  const home = (!windows && absolute(options.env.HOME)) || options.accountHome;
  const inHome = (...parts: string[]) => (path.isAbsolute(home) ? [path.join(home, ...parts)] : []);
  const key = (entry: string) => {
    const normal = path.normalize(entry).replace(windows ? /[\\/]+$/ : /\/+$/, "") || path.sep;
    return windows ? normal.toLowerCase() : normal;
  };
  const entries = [...(options.bundled ?? [])];
  if (options.env.PATH) entries.push(...options.env.PATH.split(path.delimiter));
  const seen = new Set(entries.filter(Boolean).map(key));
  // Always on Desktop's PATH, there or not, so a tool installed into one
  // while the server runs is found as it was before.
  for (const candidate of [
    ...inHome(".bun", "bin"),
    ...(windows ? [] : ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]),
  ])
    if (!seen.has(key(candidate))) {
      seen.add(key(candidate));
      entries.push(candidate);
    }
  const PATH = entries.join(path.delimiter);
  if (windows) return { PATH };

  const me = options.uid ?? process.getuid?.() ?? -1;
  const theirs = (candidate: string) => {
    const found = folder(candidate);
    return !!found?.directory && (found.uid === me || found.uid === 0) && (found.mode & 0o022) === 0;
  };
  const shared = (candidate: string) => {
    const found = folder(candidate);
    return !!found?.directory && (found.mode & 0o002) === 0;
  };
  const named = (folders: (string | { macOS: string })[]) =>
    folders.flatMap((folder) => (typeof folder === "string" ? [folder] : macOS ? [folder.macOS] : []));
  const nvmRoot =
    absolute(options.env.NVM_DIR) ??
    [
      ...(absolute(options.env.XDG_CONFIG_HOME) ? [path.join(options.env.XDG_CONFIG_HOME!, "nvm")] : []),
      ...NVM.flatMap((root) => inHome(root)),
    ].find((root) => folder(root)?.directory) ??
    inHome(".nvm")[0];
  const personal = (folders: string[]) => folders.map((folder) => [folder, theirs] as const);
  const candidates = [
    ...personal(named(PERSONAL).flatMap((folder) => inHome(folder))),
    // Homebrew's `sbin`: Apple silicon's and Intel's, or Linux's own
    // folders. `/home` is looked at on Linux alone: on a Mac it is an
    // automount, which may wait on the network.
    ...(macOS ? ["/opt/homebrew/sbin", "/usr/local/sbin"] : [])
      .concat(options.platform === "linux" ? ["/home/linuxbrew/.linuxbrew/bin", "/home/linuxbrew/.linuxbrew/sbin"] : [])
      .map((folder) => [folder, shared] as const),
    // Version managers last: each answers only a name nothing before has,
    // so a system or Homebrew tool is never replaced by one.
    ...personal([
      ...named(MANAGERS).flatMap((folder) => inHome(folder)),
      ...named(FNM).flatMap((root) => inHome(root, "aliases", "default", "bin")),
      ...(nvmRoot ? nvmFolders(path, nvmRoot, options.files ?? disk) : []),
      ...named(AFTER_NVM).flatMap((folder) => inHome(folder)),
    ]),
  ];
  const tools: string[] = [];
  // Folders the person named in the variable themselves come first.
  for (const own of (options.env.HAARNESS_TOOL_PATH ?? "").split(path.delimiter))
    if (absolute(own) && !seen.has(key(own))) {
      seen.add(key(own));
      tools.push(own);
    }
  for (const [candidate, usable] of candidates)
    if (!seen.has(key(candidate)) && usable(candidate)) {
      seen.add(key(candidate));
      tools.push(candidate);
    }
  return tools.length ? { PATH, HAARNESS_TOOL_PATH: tools.join(path.delimiter) } : { PATH };
}
