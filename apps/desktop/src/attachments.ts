export interface Attachment {
  name: string;
  data: string;
}
export const attachmentMimeTypes = new Set([
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  "application/pdf",
  "audio/mpeg",
  "audio/wav",
  "audio/ogg",
  "audio/flac",
  "audio/mp4",
  "video/mp4",
  "video/webm",
  "video/quicktime",
]);
export function checkAttachments(values: Attachment[]) {
  if (
    values.length > 20 ||
    values.reduce(
      (total, value) => {
        const encoded = value.data.split(",")[1] ?? "";
        return total + Math.floor(encoded.length / 4) * 3 - (encoded.match(/=+$/)?.[0].length ?? 0);
      },
      0,
    ) >
      20 * 1024 * 1024
  )
    throw new Error("Attachments must total at most 20 MiB and 20 files.");
  return values;
}

export function readAttachments(value: unknown): Attachment[] {
  if (!Array.isArray(value) || !value.every((item) =>
    item && typeof item.name === "string" && item.name.length <= 4096 &&
    typeof item.data === "string" && attachmentMimeTypes.has(item.data.match(/^data:([^;,]+)/)?.[1] ?? "") &&
    /^data:[^,]+;base64,[A-Za-z0-9+/]*={0,2}$/.test(item.data)))
    throw new Error("Invalid attachment draft");
  return checkAttachments(value);
}
