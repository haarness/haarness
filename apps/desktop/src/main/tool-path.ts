import { opendirSync, readFileSync, statSync } from "node:fs";
import { posix, win32 } from "node:path";

/** What PATH needs to know of a folder; nothing when it is not there. */
export type FolderCheck = (path: string) => { directory: boolean; openToAll: boolean } | undefined;

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
    return { directory: stat.isDirectory(), openToAll: (stat.mode & 0o002) !== 0 };
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

/** `20.18.1`, or a part of one such as `20`, as numbers. */
function versionNumbers(text: string) {
  const parts = text.split(".");
  return parts.every((part) => /^\d+$/.test(part)) ? parts.map(Number) : undefined;
}

/**
 * The folders of the node a new terminal gets from nvm, read from its
 * folder without running anything: its `current` link (kept when
 * `NVM_SYMLINK_CURRENT` is on), then the installed version its `default`
 * alias names (`18`, `v20.11`, an exact version, or an alias such as
 * `lts/*` that leads to one of those, a few aliases deep at most). `node`,
 * an alias naming nothing installed, or no alias at all give the newest
 * installed version; `system` gives none.
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
 * The PATH Desktop starts Haarness with. An app opened from the Dock or the
 * Finder gets only the system's folders (a desktop launcher on Linux, the
 * session's), and the agent's commands run under `sh -l`, which reads
 * `~/.profile` but never a zsh or bash profile: tools that a terminal finds
 * through those (uv, ruff, rust-analyzer, gopls, Homebrew's `sbin`) would be
 * missing. So after the app's own programs and the PATH it was started with,
 * kept as it is, come the folders Desktop has always added, then the usual
 * homes of tools people install themselves and the node nvm would choose,
 * each only when it is there, absolute, not open for everyone to write in,
 * and not on the PATH already. Nothing is run to find them. Windows apps get
 * the person's PATH from the registry, so there only Bun's folder follows,
 * as before.
 */
export function toolPath(options: {
  platform: NodeJS.Platform;
  /** Desktop's own environment: its `PATH`, and `HOME` and `NVM_DIR` outside Windows. */
  env: { PATH?: string; HOME?: string; NVM_DIR?: string };
  /** The account's home folder, for an environment without a usable `HOME`. */
  accountHome: string;
  /** Folders ahead of everything (a packaged app's own programs). */
  bundled?: string[];
  folder?: FolderCheck;
  files?: Files;
}): string {
  const windows = options.platform === "win32";
  const path = windows ? win32 : posix;
  const folder = options.folder ?? inspect;
  // The home the agent's shell sees, which a test or a terminal may move.
  const home =
    !windows && options.env.HOME && path.isAbsolute(options.env.HOME) ? options.env.HOME : options.accountHome;
  const inHome = (...parts: string[]) => (path.isAbsolute(home) ? [path.join(home, ...parts)] : []);
  const key = (entry: string) => {
    const normal = path.normalize(entry).replace(windows ? /[\\/]+$/ : /\/+$/, "") || path.sep;
    return windows ? normal.toLowerCase() : normal;
  };
  const entries = [...(options.bundled ?? [])];
  if (options.env.PATH) entries.push(...options.env.PATH.split(path.delimiter));
  const seen = new Set(entries.filter(Boolean).map(key));
  const add = (candidate: string, usable: (candidate: string) => boolean) => {
    if (seen.has(key(candidate)) || !usable(candidate)) return;
    seen.add(key(candidate));
    entries.push(candidate);
  };
  // Always on Desktop's PATH, there or not, so a tool installed into one
  // while the server runs is found as it was before.
  for (const candidate of [
    ...inHome(".bun", "bin"),
    ...(windows ? [] : ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]),
  ])
    add(candidate, () => true);
  if (windows) return entries.join(path.delimiter);
  const nvm =
    options.env.NVM_DIR && path.isAbsolute(options.env.NVM_DIR) ? options.env.NVM_DIR : inHome(".nvm")[0];
  const usable = (candidate: string) => {
    const found = folder(candidate);
    return !!found?.directory && !found.openToAll;
  };
  for (const candidate of [
    ...inHome(".local", "bin"),
    ...inHome(".cargo", "bin"),
    ...inHome("go", "bin"),
    ...inHome(".deno", "bin"),
    ...inHome("bin"),
    // Homebrew's own `sbin`, on Apple silicon, Intel and Linux.
    "/opt/homebrew/sbin",
    "/usr/local/sbin",
    "/home/linuxbrew/.linuxbrew/bin",
    "/home/linuxbrew/.linuxbrew/sbin",
    // Version managers last: each answers only a name nothing above has, so
    // a system or Homebrew tool is never replaced by one.
    ...inHome(".volta", "bin"),
    ...inHome(".asdf", "shims"),
    ...inHome(".local", "share", "mise", "shims"),
    ...inHome(".pyenv", "shims"),
    ...inHome(".rbenv", "shims"),
    ...inHome(".nodenv", "shims"),
    ...(nvm ? nvmFolders(path, nvm, options.files ?? disk) : []),
  ])
    add(candidate, usable);
  return entries.join(path.delimiter);
}
