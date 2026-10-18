import { SCREENSHOT_LIMIT } from "../screenshots";
import { imageSize, type ImageSize } from "./image-size";
import { openListed, type FileIdentity } from "./screenshot-files";

/** A thumbnail's box: two device pixels for each CSS pixel of a tile, in the tile's 4:3 shape. */
export const THUMBNAIL = { width: 280, height: 210 };
/** Largest image macOS's thumbnailer is asked to draw; it works outside the app. */
export const SYSTEM_THUMBNAIL_PIXELS = 100_000_000;
/** Largest image drawn inside the app (about a 6K screen): decoding it blocks the main process. */
export const DECODED_THUMBNAIL_PIXELS = 25_000_000;
/** Larger files get no thumbnail. */
export const THUMBNAIL_BYTES = 64 * 1024 * 1024;

/** The few `nativeImage` calls thumbnails need, so tests can stand in for Electron. */
export interface ThumbnailImage {
  isEmpty(): boolean;
  getSize(): ImageSize;
  resize(options: { width: number; height: number }): ThumbnailImage;
  toDataURL(): string;
}
export interface ThumbnailImages {
  createThumbnailFromPath(path: string, size: ImageSize): Promise<ThumbnailImage>;
  createFromPath(path: string): ThumbnailImage;
}

/** An image's thumbnail size: within the box, in the image's own shape, never enlarged. */
export function thumbnailSize(image: ImageSize, box = THUMBNAIL): ImageSize {
  const scale = Math.min(1, box.width / image.width, box.height / image.height);
  return {
    width: Math.max(1, Math.round(image.width * scale)),
    height: Math.max(1, Math.round(image.height * scale)),
  };
}

/**
 * Who may draw an image's thumbnail. macOS's thumbnailer works outside the
 * app and asynchronously; Windows's may decode in the app, so it gets the
 * same limit as decoding here (Linux has only that). Past both limits the
 * image gets none.
 */
export function thumbnailPlan(platform: NodeJS.Platform, image: ImageSize) {
  const pixels = image.width * image.height;
  return {
    system:
      (platform === "darwin" && pixels <= SYSTEM_THUMBNAIL_PIXELS) ||
      (platform === "win32" && pixels <= DECODED_THUMBNAIL_PIXELS),
    decode: pixels <= DECODED_THUMBNAIL_PIXELS,
  };
}

/**
 * Thumbnails for listed screenshots, a few at a time. Each image's size is
 * read from its header first, so one too large to draw safely (a small file
 * can unpack to gigabytes) is never decoded, and the system's thumbnailer is
 * asked for the image's own shape (macOS draws whatever box it is given).
 * Thumbnails are kept while their file stays the same, so opening the dialog
 * again draws only new screenshots. One that cannot be drawn is empty.
 */
export function createScreenshotThumbnails(options: {
  platform: NodeJS.Platform;
  images: ThumbnailImages;
  /** Thumbnails kept, the most recently shown ones. */
  keep?: number;
}) {
  const cache = new Map<string, string>();
  const keep = options.keep ?? SCREENSHOT_LIMIT * 2;

  /** The thumbnail, or undefined when the file is no longer the listed one (or cannot be read). */
  async function draw(path: string, listed: FileIdentity): Promise<string | undefined> {
    let image: ImageSize | undefined;
    // Its header is read only while it is the listed file, never through a link.
    const file = await openListed(path, listed);
    if (!file) return undefined;
    try {
      if (listed.size > THUMBNAIL_BYTES) return "";
      image = await imageSize(async (position, length) => {
        const buffer = Buffer.alloc(length);
        const { bytesRead } = await file.read(buffer, 0, length, position);
        return buffer.subarray(0, bytesRead);
      }, Number(listed.size));
    } finally {
      await file.close();
    }
    if (!image) return "";
    const plan = thumbnailPlan(options.platform, image);
    if (plan.system) {
      const drawn = await options.images.createThumbnailFromPath(path, thumbnailSize(image)).catch(() => undefined);
      if (drawn && !drawn.isEmpty()) return drawn.toDataURL();
    }
    if (!plan.decode) return "";
    // Decoding blocks the main process: let waiting IPC and terminal output through first.
    await new Promise((resolve) => setImmediate(resolve));
    const decoded = options.images.createFromPath(path);
    if (decoded.isEmpty()) return "";
    const natural = decoded.getSize();
    const size = thumbnailSize(natural);
    return (size.width < natural.width ? decoded.resize(size) : decoded).toDataURL();
  }

  return async function thumbnails(entries: { path: string; identity: FileIdentity }[]) {
    const output: string[] = new Array(entries.length).fill("");
    let next = 0;
    await Promise.all(
      Array.from({ length: Math.min(4, entries.length) }, async () => {
        while (next < entries.length) {
          const index = next++;
          const { path, identity: listed } = entries[index];
          const key = [listed.dev, listed.ino, listed.size, listed.mtimeNs, path].join("\0");
          let value = cache.get(key);
          cache.delete(key);
          if (value === undefined) value = await draw(path, listed).catch(() => undefined);
          if (value === undefined) continue;
          // The most recently shown last; the oldest go first.
          cache.set(key, value);
          for (const old of cache.keys()) {
            if (cache.size <= keep) break;
            cache.delete(old);
          }
          output[index] = value;
        }
      }),
    );
    return output;
  };
}
