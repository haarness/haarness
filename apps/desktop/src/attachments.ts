export interface Attachment {
  name: string;
  data: string;
}
export const attachmentMimeTypes = new Set([
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  // The server converts these to PNG or JPEG.
  "image/bmp",
  "image/tiff",
  "image/heic",
  "image/heif",
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
/**
 * The image type a file's first bytes announce. Browsers and file pickers go
 * by the name, and a JPEG saved as `shot.png` is refused by providers when it
 * is sent as PNG; the server checks again and converts what it must.
 */
export function imageType(bytes: Uint8Array): string | undefined {
  const text = (start: number, end: number) => String.fromCharCode(...bytes.subarray(start, end));
  if (bytes[0] === 0x89 && text(1, 8) === "PNG\r\n\x1a\n") return "image/png";
  if (bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) return "image/jpeg";
  if (text(0, 6) === "GIF87a" || text(0, 6) === "GIF89a") return "image/gif";
  if (text(0, 4) === "RIFF" && text(8, 12) === "WEBP") return "image/webp";
  if (text(0, 2) === "BM" && bytes.length >= 26) return "image/bmp";
  if (text(0, 4) === "II*\0" || text(0, 4) === "MM\0*") return "image/tiff";
  if (text(4, 8) === "ftyp") {
    const brand = text(8, 12);
    if (["heic", "heix", "heim", "heis", "hevc", "hevx", "hevm", "hevs"].includes(brand)) return "image/heic";
    if (["mif1", "msf1"].includes(brand)) return "image/heif";
  }
  return undefined;
}

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
