/**
 * An image's size from its header, without decoding it. The screenshot picker
 * checks it before drawing a thumbnail, so a small file that unpacks to
 * gigabytes of pixels never reaches a decoder, and asks the system's
 * thumbnailer for the image's own shape. Covers the types the picker lists;
 * anything it cannot read is unknown.
 */

/** Reads up to `length` bytes at `position`; fewer at the end of the file. */
export type ReadAt = (position: number, length: number) => Promise<Uint8Array>;
export type ImageSize = { width: number; height: number };

/** Box and segment walks stop after this many steps, so a crafted file cannot loop. */
const STEPS = 1024;
/** HEIF's `meta` box (item and property tables) is read whole up to this size. */
const META_LIMIT = 4 * 1024 * 1024;

const ascii = (bytes: Uint8Array, start: number, end: number) =>
  String.fromCharCode(...bytes.subarray(start, end));
const view = (bytes: Uint8Array) => new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
const fromBytes =
  (bytes: Uint8Array): ReadAt =>
  async (position, length) =>
    bytes.subarray(position, position + length);

/** A size, turned a quarter when the file says it is shown rotated. */
function sized(width: number, height: number, quarterTurn = false): ImageSize | undefined {
  if (!(width > 0 && height > 0)) return undefined;
  return quarterTurn ? { width: height, height: width } : { width, height };
}

/** The display size of the image `read` reads, or undefined when its header is not one these types have. */
export async function imageSize(read: ReadAt, fileSize: number): Promise<ImageSize | undefined> {
  try {
    return await headerSize(read, fileSize);
  } catch {
    // A header cut short reads past its end.
    return undefined;
  }
}

async function headerSize(read: ReadAt, fileSize: number) {
  const head = await read(0, 32);
  if (head.length < 26) return undefined;
  const data = view(head);
  if (head[0] === 0x89 && ascii(head, 1, 8) === "PNG\r\n\x1a\n")
    return ascii(head, 12, 16) === "IHDR" ? sized(data.getUint32(16), data.getUint32(20)) : undefined;
  if (head[0] === 0xff && head[1] === 0xd8 && head[2] === 0xff) return jpegSize(read, fileSize);
  if (ascii(head, 0, 6) === "GIF87a" || ascii(head, 0, 6) === "GIF89a")
    return sized(data.getUint16(6, true), data.getUint16(8, true));
  if (ascii(head, 0, 4) === "RIFF" && ascii(head, 8, 12) === "WEBP") return webpSize(head);
  if (ascii(head, 0, 2) === "BM") {
    // The old OS/2 header has 16-bit sizes; every later one 32-bit, negative for top-down rows.
    if (data.getUint32(14, true) === 12) return sized(data.getUint16(18, true), data.getUint16(20, true));
    return sized(Math.abs(data.getInt32(18, true)), Math.abs(data.getInt32(22, true)));
  }
  if (ascii(head, 0, 4) === "II*\0" || ascii(head, 0, 4) === "MM\0*") {
    const tags = await tiffTags(read);
    return tags && sized(tags.width ?? 0, tags.height ?? 0, (tags.orientation ?? 1) >= 5);
  }
  if (ascii(head, 4, 8) === "ftyp") return heifSize(read, fileSize);
  return undefined;
}

function webpSize(head: Uint8Array) {
  const data = view(head);
  switch (ascii(head, 12, 16)) {
    case "VP8 ": // Lossy: a key frame's start code, then 14-bit sizes.
      if (head[23] !== 0x9d || head[24] !== 0x01 || head[25] !== 0x2a || head.length < 30) return undefined;
      return sized(data.getUint16(26, true) & 0x3fff, data.getUint16(28, true) & 0x3fff);
    case "VP8L": {
      // Lossless: a signature byte, then width - 1 and height - 1 in 14 bits each.
      if (head[20] !== 0x2f || head.length < 25) return undefined;
      const bits = data.getUint32(21, true);
      return sized((bits & 0x3fff) + 1, ((bits >>> 14) & 0x3fff) + 1);
    }
    case "VP8X": // Extended: the canvas, width - 1 and height - 1 in 24 bits each.
      if (head.length < 30) return undefined;
      return sized(
        (head[24] | (head[25] << 8) | (head[26] << 16)) + 1,
        (head[27] | (head[28] << 8) | (head[29] << 16)) + 1,
      );
  }
  return undefined;
}

/** The first directory's size and orientation tags of the TIFF structure `read` reads. */
async function tiffTags(read: ReadAt) {
  const head = await read(0, 8);
  if (head.length < 8) return undefined;
  const little = head[0] === 0x49;
  const directory = view(head).getUint32(4, little);
  const count = view(await read(directory, 2)).getUint16(0, little);
  if (!count || count > 4096) return undefined;
  const entries = await read(directory + 2, count * 12);
  const data = view(entries);
  const tags: { width?: number; height?: number; orientation?: number } = {};
  for (let offset = 0; offset + 12 <= entries.length; offset += 12) {
    const tag = data.getUint16(offset, little);
    const type = data.getUint16(offset + 2, little);
    // SHORT and LONG values sit in the entry itself.
    const value =
      type === 3 ? data.getUint16(offset + 8, little) : type === 4 ? data.getUint32(offset + 8, little) : undefined;
    if (value === undefined) continue;
    if (tag === 256) tags.width = value;
    else if (tag === 257) tags.height = value;
    else if (tag === 274) tags.orientation = value;
  }
  return tags;
}

/** A JPEG's frame size, from the first start-of-frame segment; Exif's orientation turns it. */
async function jpegSize(read: ReadAt, fileSize: number) {
  let position = 2;
  let orientation = 1;
  for (let step = 0; step < STEPS && position + 4 <= fileSize; step++) {
    const marker = await read(position, 4);
    if (marker.length < 4 || marker[0] !== 0xff) return undefined;
    const type = marker[1];
    if (type === 0xff) {
      position++; // Fill byte.
      continue;
    }
    if (type === 0x01 || (type >= 0xd0 && type <= 0xd8)) {
      position += 2; // A marker without a length.
      continue;
    }
    // The image data starts, or ends, before any frame: nothing to read.
    if (type === 0xd9 || type === 0xda) return undefined;
    const length = view(marker).getUint16(2);
    if (length < 2) return undefined;
    // Start of frame, any coding (C4, C8 and CC are other tables).
    if (type >= 0xc0 && type <= 0xcf && type !== 0xc4 && type !== 0xc8 && type !== 0xcc) {
      const frame = await read(position + 5, 4);
      if (frame.length < 4) return undefined;
      return sized(view(frame).getUint16(2), view(frame).getUint16(0), orientation >= 5);
    }
    if (type === 0xe1) {
      const segment = await read(position + 4, length - 2);
      if (ascii(segment, 0, 6) === "Exif\0\0")
        orientation = (await tiffTags(fromBytes(segment.subarray(6))).catch(() => undefined))?.orientation ?? 1;
    }
    position += 2 + length;
  }
  return undefined;
}

/** The boxes in `bytes` between `start` and `end`: type and where each one's content lies. */
function* boxes(bytes: Uint8Array, start: number, end: number) {
  const data = view(bytes);
  for (let position = start, step = 0; position + 8 <= end && step < STEPS; step++) {
    let size = data.getUint32(position);
    let header = 8;
    if (size === 1) {
      if (position + 16 > end) return;
      size = data.getUint32(position + 8) * 2 ** 32 + data.getUint32(position + 12);
      header = 16;
    } else if (size === 0) size = end - position;
    if (size < header || position + size > end) return;
    yield { type: ascii(bytes, position + 4, position + 8), start: position + header, end: position + size };
    position += size;
  }
}

/**
 * A HEIF image's size: the largest `ispe` among its item properties (the
 * whole picture, not one of its tiles), turned a quarter when an `irot` says so.
 */
async function heifSize(read: ReadAt, fileSize: number) {
  let position = 0;
  for (let step = 0; step < STEPS && position + 8 <= fileSize; step++) {
    const header = await read(position, 16);
    if (header.length < 8) return undefined;
    const data = view(header);
    let size = data.getUint32(0);
    let length = 8;
    if (size === 1) {
      if (header.length < 16) return undefined;
      size = data.getUint32(8) * 2 ** 32 + data.getUint32(12);
      length = 16;
    } else if (size === 0) size = fileSize - position;
    if (size < length) return undefined;
    if (ascii(header, 4, 8) === "meta") {
      if (size > META_LIMIT) return undefined;
      const meta = await read(position + length, size - length);
      // `meta` is a full box: a version and flags before its children.
      let best: ImageSize | undefined;
      let quarterTurn = false;
      for (const properties of boxes(meta, 4, meta.length)) {
        if (properties.type !== "iprp") continue;
        for (const container of boxes(meta, properties.start, properties.end)) {
          if (container.type !== "ipco") continue;
          for (const property of boxes(meta, container.start, container.end)) {
            if (property.type === "ispe" && property.end - property.start >= 12) {
              const width = view(meta).getUint32(property.start + 4);
              const height = view(meta).getUint32(property.start + 8);
              if (!best || width * height > best.width * best.height) best = { width, height };
            } else if (property.type === "irot" && property.end > property.start)
              quarterTurn ||= (meta[property.start] & 3) % 2 === 1;
          }
        }
      }
      return best && sized(best.width, best.height, quarterTurn);
    }
    position += size;
  }
  return undefined;
}
