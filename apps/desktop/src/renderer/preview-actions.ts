export interface PreviewInfo {
  path: string;
  kind: string;
  version: string;
  title: string;
  mode: "preview" | "code";
}
export function previewInfo(metadata: unknown): PreviewInfo | undefined {
  if (!metadata || typeof metadata !== "object" || !("preview" in metadata))
    return;
  const value = metadata.preview;
  if (!value || typeof value !== "object") return;
  const info = value as Record<string, unknown>;
  if (
    typeof info.path !== "string" ||
    typeof info.version !== "string" ||
    !/^[a-f0-9]{64}$/.test(info.version)
  )
    return;
  return {
    path: info.path,
    version: info.version,
    kind: String(info.kind ?? ""),
    title: String(info.title ?? info.path),
    mode: info.mode === "code" ? "code" : "preview",
  };
}

// Coalesce status reads for banners showing several versions of the same file.
// Never cache file bodies, and keep neither snapshots nor workspace state indefinitely.
const pending = new Map<string, Promise<{ revision: string | null }>>();
export function currentPreview(connection: string, path: string) {
  const key = JSON.stringify([connection, path]);
  const existing = pending.get(key);
  if (existing) return existing;
  const request = window.haarness.files<{ revision: string | null }>(
    connection,
    "previewCurrent",
    { path },
  );
  pending.set(key, request);
  void request
    .finally(() => {
      if (pending.get(key) === request) pending.delete(key);
    })
    .catch(() => {});
  return request;
}
