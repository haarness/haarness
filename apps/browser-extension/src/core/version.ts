/**
 * Chrome versions are up to four dot-separated integers, so the workspace's
 * semantic version maps onto them: `X.Y.Z-preview.N` → `X.Y.Z.N` and a release
 * `X.Y.Z` → `X.Y.Z.1000`, which sorts after every preview of the same release.
 * The original string stays visible as `version_name`.
 */

export function manifestVersion(workspace: string): string {
  const match = /^(\d+)\.(\d+)\.(\d+)(?:-[a-z]+\.(\d+))?$/.exec(workspace.trim());
  if (!match) throw new Error(`Cannot map version ${JSON.stringify(workspace)} to a Chrome extension version`);
  const [, major, minor, patch, preview] = match;
  if (preview !== undefined && Number(preview) >= 1000)
    throw new Error(`Preview number ${preview} must be below 1000 to sort before the release`);
  const parts = [major, minor, patch, preview ?? "1000"].map(Number);
  if (parts.some((part) => part > 65_535)) throw new Error(`Version ${workspace} is out of range for Chrome`);
  return parts.join(".");
}

/** Compares two workspace versions (or manifest versions); negative when `a` is older. */
export function compareVersions(a: string, b: string): number {
  const numbers = (value: string) => {
    try {
      return manifestVersion(value).split(".").map(Number);
    } catch {
      return value.split(".").map((part) => Number.parseInt(part, 10) || 0);
    }
  };
  const left = numbers(a),
    right = numbers(b);
  for (let index = 0; index < Math.max(left.length, right.length); index++) {
    const difference = (left[index] ?? 0) - (right[index] ?? 0);
    if (difference) return difference;
  }
  return 0;
}
