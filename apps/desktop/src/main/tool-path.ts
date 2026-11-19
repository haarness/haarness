import { statSync } from "node:fs";
import { posix, win32 } from "node:path";

/** What PATH needs to know of a folder; nothing when it is not there. */
export type FolderCheck = (path: string) => { directory: boolean; openToAll: boolean } | undefined;

function inspect(path: string) {
  try {
    const stat = statSync(path);
    return { directory: stat.isDirectory(), openToAll: (stat.mode & 0o002) !== 0 };
  } catch {
    return undefined;
  }
}

/**
 * The PATH Desktop starts Haarness with. An app opened from the Dock or the
 * Finder gets only the system's folders (a desktop launcher on Linux, the
 * session's), and the agent's commands run under `sh -l`, which reads
 * `~/.profile` but never a zsh or bash profile: tools that a terminal finds
 * through those (uv, ruff, rust-analyzer, gopls, Homebrew's `sbin`) would be
 * missing. So after the app's own programs and the PATH it was started with,
 * kept as it is, come the folders Desktop has always added, then the usual
 * homes of tools people install themselves, each only when it is there,
 * absolute, not open for everyone to write in, and not on the PATH already.
 * Nothing is run to find them. Windows apps get the person's PATH from the
 * registry, so there only Bun's folder follows, as before.
 */
export function toolPath(options: {
  platform: NodeJS.Platform;
  /** Desktop's own environment: its `PATH`, and `HOME` outside Windows. */
  env: { PATH?: string; HOME?: string };
  /** The account's home folder, for an environment without a usable `HOME`. */
  accountHome: string;
  /** Folders ahead of everything (a packaged app's own programs). */
  bundled?: string[];
  folder?: FolderCheck;
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
    // Version managers' shims last: each answers only a name nothing above
    // has, so a system or Homebrew tool is never replaced by one.
    ...inHome(".volta", "bin"),
    ...inHome(".asdf", "shims"),
    ...inHome(".local", "share", "mise", "shims"),
    ...inHome(".pyenv", "shims"),
    ...inHome(".rbenv", "shims"),
    ...inHome(".nodenv", "shims"),
  ])
    add(candidate, usable);
  return entries.join(path.delimiter);
}
