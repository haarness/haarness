/**
 * Where a clicked link in rendered Markdown (a reply, a previewed Markdown
 * file) may go. Web and mail links leave through the desktop's external-link
 * check. A link to a file inside the project opens in the Files panel, where
 * HTML and SVG render only in the sandboxed preview, so nothing a link names
 * reaches the system's file opener: a link cannot launch an app or run a
 * script. A network location is refused before anything resolves it, since
 * resolving one already reaches its host. Pure, so it is tested without a
 * window.
 */
export type LinkTarget =
  | { kind: "web"; url: string }
  /** `path` is relative to the project, `/`-separated; "" is the project itself. */
  | { kind: "file"; path: string; line?: number }
  /** An empty link, or a fragment within the same text: nothing to open. */
  | { kind: "none" }
  | { kind: "refused"; reason: "network" | "outside" | "unsupported" };

export interface LinkPlace {
  /** The project's folder, absolute, as the server reports it. */
  root: string;
  /** The folder relative links start from: relative to `root` or absolute. Defaults to `root`. */
  base?: string;
}

/** Schemes that name a file share on another computer. */
const NETWORK = new Set(["smb", "afp", "nfs", "cifs", "dav", "davs", "webdav", "webdavs"]);

const refused = (reason: "network" | "outside" | "unsupported"): LinkTarget => ({
  kind: "refused",
  reason,
});

/** Two leading slashes or backslashes in any mix: `//host/share`, `\\host\share`. */
const networkPath = (path: string) => /^[\\/]{2}/.test(path);

export function linkTarget(href: string, place: LinkPlace): LinkTarget {
  // Read the link the way the URL parser does: it drops leading and trailing
  // spaces and control characters, and any tab or newline.
  const raw = href
    .replace(/[\t\n\r]/g, "")
    .replace(/^[\x00-\x20]+|[\x00-\x20]+$/g, "");
  if (!raw || raw.startsWith("#")) return { kind: "none" };
  if (networkPath(raw)) return refused("network");
  const scheme = /^([a-z][a-z\d+.-]*):/i.exec(raw)?.[1]?.toLowerCase();
  // A drive letter (`C:\`, `c:/`) starts a Windows path, not a scheme.
  if (scheme && !(scheme.length === 1 && /^[a-z]:[\\/]/i.test(raw))) {
    if (scheme === "http" || scheme === "https" || scheme === "mailto")
      return { kind: "web", url: raw };
    if (scheme === "file") return fileURL(raw, place);
    return refused(NETWORK.has(scheme) ? "network" : "unsupported");
  }
  const hash = raw.indexOf("#");
  const fragment = hash >= 0 ? raw.slice(hash) : "";
  const body = (hash >= 0 ? raw.slice(0, hash) : raw).replace(/\?.*$/, "");
  return filePath(decode(body), fragment, place);
}

function fileURL(raw: string, place: LinkPlace): LinkTarget {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return refused("unsupported");
  }
  // Another host is a network share.
  if (url.hostname && url.hostname !== "localhost") return refused("network");
  let path = decode(url.pathname);
  if (/^\/[a-z]:[\\/]/i.test(path)) path = path.slice(1);
  if (!/^(?:\/|[a-z]:[\\/])/i.test(path)) return refused("unsupported");
  return filePath(path, url.hash, place);
}

/** Percent-decoding, as Markdown writes a link's spaces and accents; malformed escapes stay. */
function decode(text: string) {
  try {
    return decodeURIComponent(text);
  } catch {
    return text;
  }
}

const absolute = (path: string) => /^(?:\/|[a-z]:\/)/i.test(path);

function filePath(input: string, fragment: string, place: LinkPlace): LinkTarget {
  if (networkPath(input)) return refused("network");
  // `#L12` (GitHub, VS Code) or a `path:12`, `path:12:4`, `path:12-20` suffix.
  let line = Number(/^#L(\d+)/i.exec(fragment)?.[1]) || undefined;
  let path = input;
  const suffix = /:(\d+)(?:[:-]\d+)?$/.exec(path);
  if (suffix) {
    line ??= Number(suffix[1]) || undefined;
    path = path.slice(0, suffix.index);
  }
  path = path.replace(/\\/g, "/");
  if (!path || path.startsWith("~")) return refused(path ? "outside" : "unsupported");
  const root = place.root.replace(/\\/g, "/").replace(/\/+$/, "");
  const start = absolute(path) ? path : join(relative(place.base ?? "", root), path);
  if (start === undefined) return refused("outside");
  const inside = absolute(start) ? relative(start, root) : start;
  const parts = inside === undefined ? undefined : normalize(inside);
  if (parts === undefined) return refused("outside");
  return { kind: "file", path: parts, ...(line ? { line } : {}) };
}

/** `path` relative to `root`, or undefined when it lies elsewhere. Relative input passes through. */
function relative(path: string, root: string): string | undefined {
  const value = path.replace(/\\/g, "/");
  if (!absolute(value)) return value;
  // Windows compares paths without case; elsewhere case is significant.
  const fold = /^[a-z]:\//i.test(root) ? (text: string) => text.toLowerCase() : (text: string) => text;
  const trimmed = value.replace(/\/+$/, "");
  if (fold(trimmed) === fold(root)) return "";
  if (!root || !fold(trimmed).startsWith(fold(root) + "/")) return undefined;
  return trimmed.slice(root.length + 1);
}

function join(base: string | undefined, path: string) {
  if (base === undefined) return undefined;
  return base ? base.replace(/\/+$/, "") + "/" + path : path;
}

/** Resolves `.` and `..`; undefined when `..` climbs above the project. */
function normalize(path: string) {
  const parts: string[] = [];
  for (const part of path.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (!parts.length) return undefined;
      parts.pop();
    } else parts.push(part);
  }
  return parts.join("/");
}
