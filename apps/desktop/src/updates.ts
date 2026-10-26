/**
 * Desktop updates from the Haarness release service, the same releases
 * `install.sh`, `install.ps1` and `haarness upgrade` install: the pure part
 * (versions, the release manifest, which file this computer takes) shared by
 * the main process and the window.
 */

/** Why a check or download failed; the window says it in the person's language. */
export type UpdateErrorCode =
  /** The release service could not be reached (offline, DNS, a timeout). */
  | "offline"
  /** It answered, but not with a release (an HTTP error, a bad manifest). */
  | "service"
  /** The download's size or SHA-256 did not match the release, from every source. */
  | "damaged"
  /** Not enough free disk space for the download and its unpacked app. */
  | "space"
  /** The download could not be written or unpacked. */
  | "write"
  /** The new app's code signature does not verify, or is another developer's. */
  | "signature"
  /** The last install after quitting did not complete; the old version stayed. */
  | "install"
  | "unknown";

/** What Desktop knows about a newer version, as the window shows it. */
export interface UpdateState {
  /**
   * What this copy can do about an update: replace itself and restart
   * (`restart`), only once it is moved to Applications (`move`, a copy run
   * from a disk image or a translocated download), only through an installer
   * the person opens (`installer`, a copy it may not write, or the .deb), or
   * nothing (`off`, a development build).
   */
  method: "restart" | "move" | "installer" | "off";
  /**
   * `available`: a newer version, not downloaded; `downloading`, then
   * `verifying` (checksum, signature, unpacking); `ready`: restarting
   * installs it; `installing`: Haarness is quitting to install it.
   */
  status: "idle" | "available" | "downloading" | "verifying" | "ready" | "installing";
  /** A check is in flight; whatever was found before stays meanwhile. */
  checking: boolean;
  /** The newer version, whenever the status is not `idle`. */
  version?: string;
  /** Of the download, 0–100, while downloading. */
  percent?: number;
  /** The release's notes, a web page, when it names one. */
  notesUrl?: string;
  /** When the last check reached the release service (ms since the epoch). */
  checkedAt?: number;
  /** Why the last check, download or install failed; `detail` is technical, for a tooltip. */
  error?: { code: UpdateErrorCode; detail?: string };
  /** The person asked for this download, so the sidebar shows its progress. */
  asked?: boolean;
}

export type UpdateAction = "check" | "download" | "restart" | "move" | "open-installer";

const SEMVER = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/;

/** A version as numbers and its prerelease identifiers, or null for none. */
function parse(value: string) {
  const match = SEMVER.exec(value.trim());
  if (!match) return null;
  return {
    core: [Number(match[1]), Number(match[2]), Number(match[3])],
    pre: match[4] ? match[4].split(".") : [],
  };
}

/** Whether `value` is a version Desktop can compare. */
export function validVersion(value: unknown): value is string {
  return typeof value === "string" && parse(value) !== null;
}

/**
 * SemVer order: negative when `a` is older than `b`, positive when newer, 0
 * when equal (build metadata does not count). A version that cannot be read
 * is older than any that can, so a malformed release never looks newer.
 */
export function compareVersions(a: string, b: string): number {
  const left = parse(a);
  const right = parse(b);
  if (!left || !right) return left ? 1 : right ? -1 : 0;
  for (let index = 0; index < 3; index++)
    if (left.core[index] !== right.core[index]) return left.core[index] - right.core[index];
  // A release outranks its own prereleases.
  if (!left.pre.length || !right.pre.length) return right.pre.length - left.pre.length;
  const numeric = /^\d+$/;
  for (let index = 0; index < Math.max(left.pre.length, right.pre.length); index++) {
    const x = left.pre[index];
    const y = right.pre[index];
    if (x === undefined) return -1;
    if (y === undefined) return 1;
    if (numeric.test(x) && numeric.test(y)) {
      if (Number(x) !== Number(y)) return Number(x) - Number(y);
    } else if (numeric.test(x) !== numeric.test(y)) return numeric.test(x) ? -1 : 1;
    else if (x !== y) return x < y ? -1 : 1;
  }
  return 0;
}

/** Whether `version` is a prerelease (`0.2.0-preview.1`). */
export function isPrerelease(version: string) {
  return (parse(version)?.pre.length ?? 0) > 0;
}

/**
 * The channels a version follows: a release build follows stable; a
 * prerelease follows preview and stable both, so it moves on to a stable
 * release that passes it.
 */
export function channelsFor(version: string): ("stable" | "preview")[] {
  return isPrerelease(version) ? ["preview", "stable"] : ["stable"];
}

export interface ReleaseArtifact {
  id: string;
  os: string;
  arch: string;
  kind: string;
  file: string;
  url: string;
  size: number;
  sha256: string;
}
export interface DesktopRelease {
  version: string;
  notesUrl?: string;
  artifacts: ReleaseArtifact[];
}

const record = (value: unknown): Record<string, unknown> | undefined =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : undefined;

/** A file name the release service would publish: no folders, nothing hidden. */
const plainFile = (value: unknown): value is string =>
  typeof value === "string" && /^[A-Za-z0-9][A-Za-z0-9._+-]{0,199}$/.test(value);

/**
 * The Desktop release in an answer of `GET /api/v1/releases/<channel>/desktop`,
 * or null when the channel has none. Artifacts that are not complete (no
 * SHA-256, no size, an odd file name) are left out rather than trusted, and an
 * answer that is not a manifest throws.
 */
export function parseRelease(body: unknown): DesktopRelease | null {
  const manifest = record(record(body)?.manifest);
  if (!manifest) throw new Error("The release service answered with something that is not a release.");
  const desktop = record(record(manifest.products)?.desktop);
  if (!desktop) return null;
  const version = desktop.version ?? manifest.version;
  if (!validVersion(version)) throw new Error("The release service named an invalid version.");
  const artifacts: ReleaseArtifact[] = [];
  for (const item of Array.isArray(desktop.artifacts) ? desktop.artifacts : []) {
    const value = record(item);
    const sha256 = record(value?.digest)?.sha256;
    if (
      !value ||
      typeof value.id !== "string" ||
      !/^[A-Za-z0-9-]{1,64}$/.test(value.id) ||
      typeof value.os !== "string" ||
      typeof value.arch !== "string" ||
      typeof value.kind !== "string" ||
      !plainFile(value.file) ||
      typeof value.url !== "string" ||
      typeof value.size !== "number" ||
      !Number.isSafeInteger(value.size) ||
      value.size <= 0 ||
      typeof sha256 !== "string" ||
      !/^[a-f0-9]{64}$/.test(sha256)
    )
      continue;
    artifacts.push({
      id: value.id,
      os: value.os,
      arch: value.arch,
      kind: value.kind,
      file: value.file,
      url: value.url,
      size: value.size,
      sha256,
    });
  }
  const notes = desktop.notes_url;
  return {
    version,
    artifacts,
    ...(typeof notes === "string" && /^https:\/\/[^\s]+$/.test(notes) ? { notesUrl: notes } : {}),
  };
}

/** This computer's file in a release: same system, architecture and kind. */
export function pickArtifact(
  release: DesktopRelease,
  target: { os: string; arch: string; kind: string },
): ReleaseArtifact | undefined {
  return release.artifacts.find(
    (item) =>
      item.os === target.os &&
      item.arch === target.arch &&
      item.kind.toLowerCase() === target.kind.toLowerCase(),
  );
}
