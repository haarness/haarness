import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm, symlink, utimes, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { imageSize, type ImageSize } from "../src/main/image-size";
import { listScreenshots } from "../src/main/screenshot-files";
import {
  createScreenshotThumbnails,
  thumbnailPlan,
  thumbnailSize,
  type ThumbnailImage,
} from "../src/main/screenshot-thumbnails";

const be32 = (value: number) => {
  const bytes = Buffer.alloc(4);
  bytes.writeUInt32BE(value);
  return bytes;
};
const be16 = (value: number) => {
  const bytes = Buffer.alloc(2);
  bytes.writeUInt16BE(value);
  return bytes;
};
const le32 = (value: number) => {
  const bytes = Buffer.alloc(4);
  bytes.writeInt32LE(value);
  return bytes;
};
const le16 = (value: number) => {
  const bytes = Buffer.alloc(2);
  bytes.writeUInt16LE(value);
  return bytes;
};
const le24 = (value: number) => Buffer.from([value & 255, (value >> 8) & 255, (value >> 16) & 255]);
const box = (type: string, ...content: Buffer[]) => {
  const body = Buffer.concat(content);
  return Buffer.concat([be32(8 + body.length), Buffer.from(type, "latin1"), body]);
};

// Headers only: nothing here is a decodable image, which is the point.
const png = (width: number, height: number) =>
  Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    be32(13),
    Buffer.from("IHDR"),
    be32(width),
    be32(height),
    Buffer.from([8, 6, 0, 0, 0]),
    be32(0),
  ]);
/** A TIFF structure with one directory; `gap` puts the directory after other bytes, as some writers do. */
const tiff = (tags: [number, number, number][], little = true, gap = 0) => {
  const u16 = (value: number) => (little ? le16(value) : be16(value));
  const u32 = (value: number) => (little ? le32(value) : be32(value));
  const entries = tags.map(([tag, type, value]) =>
    Buffer.concat([u16(tag), u16(type), u32(1), type === 3 ? Buffer.concat([u16(value), u16(0)]) : u32(value)]),
  );
  return Buffer.concat([
    Buffer.from(little ? "II*\0" : "MM\0*", "latin1"),
    u32(8 + gap),
    Buffer.alloc(gap),
    u16(tags.length),
    ...entries,
    u32(0),
  ]);
};
const jpeg = (width: number, height: number, orientation?: number) =>
  Buffer.concat([
    Buffer.from([0xff, 0xd8]),
    ...(orientation
      ? (() => {
          const exif = Buffer.concat([Buffer.from("Exif\0\0", "latin1"), tiff([[274, 3, orientation]])]);
          return [Buffer.from([0xff, 0xe1]), be16(exif.length + 2), exif];
        })()
      : []),
    // A quantisation table to step over, then the frame.
    Buffer.from([0xff, 0xdb]),
    be16(6),
    Buffer.alloc(4),
    Buffer.from([0xff, 0xc2]),
    be16(17),
    Buffer.from([8]),
    be16(height),
    be16(width),
    Buffer.alloc(10),
    Buffer.from([0xff, 0xda]),
  ]);
const riff = (chunk: string, body: Buffer) =>
  Buffer.concat([Buffer.from("RIFF"), le32(4 + 8 + body.length), Buffer.from("WEBP"), Buffer.from(chunk), le32(body.length), body]);
const heic = (width: number, height: number, quarterTurn = false) =>
  Buffer.concat([
    box("ftyp", Buffer.from("heic"), be32(0), Buffer.from("mif1heic")),
    box(
      "meta",
      be32(0),
      box("hdlr", be32(0), be32(0), Buffer.from("pict"), Buffer.alloc(13)),
      box(
        "iprp",
        box(
          "ipco",
          // A tile's size comes first; the whole picture is the larger one.
          box("ispe", be32(0), be32(512), be32(512)),
          box("ispe", be32(0), be32(width), be32(height)),
          ...(quarterTurn ? [box("irot", Buffer.from([1]))] : []),
        ),
        box("ipma", be32(0), be32(0)),
      ),
    ),
    box("mdat", Buffer.alloc(16)),
  ]);

const sizeOf = (bytes: Buffer) =>
  imageSize(async (position, length) => bytes.subarray(position, position + length), bytes.length);

test("an image's size comes from its header, turned when it is shown rotated", async () => {
  const cases: [string, Buffer, ImageSize][] = [
    ["png", png(1600, 1000), { width: 1600, height: 1000 }],
    ["gif", Buffer.concat([Buffer.from("GIF89a"), le16(320), le16(200), Buffer.alloc(20)]), { width: 320, height: 200 }],
    [
      "bmp, rows top-down",
      Buffer.concat([Buffer.from("BM"), Buffer.alloc(12), le32(40), le32(640), le32(-480), Buffer.alloc(24)]),
      { width: 640, height: 480 },
    ],
    [
      "webp, lossy",
      riff("VP8 ", Buffer.concat([Buffer.from([0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a]), le16(300), le16(150), Buffer.alloc(8)])),
      { width: 300, height: 150 },
    ],
    [
      "webp, lossless",
      riff("VP8L", Buffer.concat([Buffer.from([0x2f]), le32(299 | (149 << 14)), Buffer.alloc(8)])),
      { width: 300, height: 150 },
    ],
    [
      "webp, extended",
      riff("VP8X", Buffer.concat([Buffer.alloc(4), le24(4999), le24(2999), Buffer.alloc(4)])),
      { width: 5000, height: 3000 },
    ],
    ["jpeg", jpeg(4032, 3024), { width: 4032, height: 3024 }],
    ["jpeg, shown a quarter turned", jpeg(4032, 3024, 6), { width: 3024, height: 4032 }],
    ["tiff", tiff([[256, 4, 2560], [257, 3, 1600]]), { width: 2560, height: 1600 }],
    ["tiff, big-endian, directory at the end", tiff([[256, 3, 800], [257, 4, 600], [274, 3, 8]], false, 5000), { width: 600, height: 800 }],
    ["heic", heic(4032, 3024), { width: 4032, height: 3024 }],
    ["heic, shown a quarter turned", heic(4032, 3024, true), { width: 3024, height: 4032 }],
    ["a size the decoder would allocate gigabytes for", png(30000, 30000), { width: 30000, height: 30000 }],
  ];
  for (const [name, bytes, expected] of cases) assert.deepEqual(await sizeOf(bytes), expected, name);
  const unknown: [string, Buffer][] = [
    ["text", Buffer.from("not an image at all, just some words")],
    ["png cut short", png(10, 10).subarray(0, 20)],
    ["png of no pixels", png(0, 10)],
    ["jpeg without a frame", Buffer.concat([Buffer.from([0xff, 0xd8, 0xff, 0xdb]), be16(6), Buffer.alloc(4), Buffer.from([0xff, 0xda]), Buffer.alloc(20)])],
    ["jpeg cut short", jpeg(100, 100).subarray(0, 14)],
    ["heic without sizes", Buffer.concat([box("ftyp", Buffer.from("heic"), be32(0)), box("meta", be32(0)), Buffer.alloc(8)])],
  ];
  for (const [name, bytes] of unknown) assert.equal(await sizeOf(bytes), undefined, name);
});

test("a thumbnail keeps its image's shape within the tile, and large images are left to the system or skipped", () => {
  assert.deepEqual(thumbnailSize({ width: 1600, height: 1000 }), { width: 280, height: 175 });
  assert.deepEqual(thumbnailSize({ width: 1000, height: 1600 }), { width: 131, height: 210 });
  assert.deepEqual(thumbnailSize({ width: 160, height: 100 }), { width: 160, height: 100 });
  assert.deepEqual(thumbnailSize({ width: 20000, height: 10 }), { width: 280, height: 1 });
  const retina = { width: 5120, height: 2880 };
  const huge = { width: 30000, height: 30000 };
  const eightK = { width: 8000, height: 6000 };
  assert.deepEqual(thumbnailPlan("darwin", retina), { system: true, decode: true });
  assert.deepEqual(thumbnailPlan("darwin", eightK), { system: true, decode: false });
  assert.deepEqual(thumbnailPlan("darwin", huge), { system: false, decode: false });
  assert.deepEqual(thumbnailPlan("win32", retina), { system: true, decode: true });
  assert.deepEqual(thumbnailPlan("win32", eightK), { system: false, decode: false });
  assert.deepEqual(thumbnailPlan("linux", retina), { system: false, decode: true });
  assert.deepEqual(thumbnailPlan("linux", eightK), { system: false, decode: false });
});

/** Stands in for Electron's nativeImage, recording what it was asked to draw. */
function fakeImages(natural: Record<string, ImageSize>, systemFails = new Set<string>()) {
  const calls: string[] = [];
  const image = (label: string, size: ImageSize, empty = false): ThumbnailImage => ({
    isEmpty: () => empty,
    getSize: () => size,
    resize: (to) => image(`${label} resized ${to.width}x${to.height}`, to),
    toDataURL: () => `data:${label}`,
  });
  return {
    calls,
    images: {
      async createThumbnailFromPath(path: string, size: ImageSize) {
        const name = basename(path);
        calls.push(`system ${name} ${size.width}x${size.height}`);
        return image(`system ${name} ${size.width}x${size.height}`, size, systemFails.has(name));
      },
      createFromPath(path: string) {
        const name = basename(path);
        calls.push(`decode ${name}`);
        return image(`decoded ${name}`, natural[name]);
      },
    },
  };
}

test("thumbnails ask the system for the image's own shape and never decode an image too large to draw safely", async () => {
  const folder = await mkdtemp(join(tmpdir(), "haarness-thumbnails-"));
  try {
    const files: [string, Buffer][] = [
      ["wide.png", png(1600, 1000)],
      ["tall.jpg", jpeg(1000, 1600)],
      ["huge.png", png(30000, 30000)],
      ["eight-k.png", png(8000, 6000)],
      ["broken.png", Buffer.from("this file only has an image's name")],
    ];
    for (const [index, [name, bytes]] of files.entries()) {
      await writeFile(join(folder, name), bytes);
      await utimes(join(folder, name), 1000 + index, 1000 + index);
    }
    const listed = await listScreenshots(folder);
    const byName = (output: string[]) => Object.fromEntries(listed.map((entry, index) => [entry.name, output[index]]));
    const natural = { "wide.png": { width: 1600, height: 1000 }, "tall.jpg": { width: 1000, height: 1600 } };

    const mac = fakeImages(natural);
    assert.deepEqual(byName(await createScreenshotThumbnails({ platform: "darwin", images: mac.images })(listed)), {
      "wide.png": "data:system wide.png 280x175",
      "tall.jpg": "data:system tall.jpg 131x210",
      "huge.png": "",
      "eight-k.png": "data:system eight-k.png 280x210",
      "broken.png": "",
    });
    assert.ok(!mac.calls.some((call) => call.startsWith("decode")), mac.calls.join());
    assert.ok(!mac.calls.some((call) => call.includes("huge") || call.includes("broken")), mac.calls.join());

    const linux = fakeImages(natural);
    assert.deepEqual(byName(await createScreenshotThumbnails({ platform: "linux", images: linux.images })(listed)), {
      "wide.png": "data:decoded wide.png resized 280x175",
      "tall.jpg": "data:decoded tall.jpg resized 131x210",
      "huge.png": "",
      "eight-k.png": "",
      "broken.png": "",
    });
    assert.deepEqual(linux.calls.sort(), ["decode tall.jpg", "decode wide.png"]);

    // When the system cannot draw one, a small enough image is decoded instead.
    const fallback = fakeImages(natural, new Set(["wide.png", "eight-k.png"]));
    const drawn = byName(await createScreenshotThumbnails({ platform: "darwin", images: fallback.images })(listed));
    assert.equal(drawn["wide.png"], "data:decoded wide.png resized 280x175");
    assert.equal(drawn["eight-k.png"], "");
    assert.ok(!fallback.calls.includes("decode eight-k.png"));
  } finally {
    await rm(folder, { recursive: true, force: true });
  }
});

test("thumbnails are kept while their file stays the same, and a swapped file is not drawn", async () => {
  const folder = await mkdtemp(join(tmpdir(), "haarness-thumbnails-"));
  try {
    await writeFile(join(folder, "a.png"), png(1600, 1000));
    await writeFile(join(folder, "b.png"), png(800, 500));
    await writeFile(join(folder, "elsewhere.png"), png(400, 250));
    const fake = fakeImages({});
    const thumbnails = createScreenshotThumbnails({ platform: "darwin", images: fake.images, keep: 3 });
    let listed = await listScreenshots(folder);
    await thumbnails(listed);
    assert.equal(fake.calls.length, 3);
    // Opening the dialog again draws nothing new.
    await thumbnails(listed);
    assert.equal(fake.calls.length, 3);
    // A changed screenshot is drawn again.
    await writeFile(join(folder, "a.png"), png(1000, 1000));
    listed = await listScreenshots(folder);
    await thumbnails(listed);
    assert.deepEqual(fake.calls.slice(3), ["system a.png 210x210"]);
    // Changed after it was listed, or made a link to another file: not drawn, and not kept.
    const a = listed.find((entry) => entry.name === "a.png")!;
    const b = listed.find((entry) => entry.name === "b.png")!;
    await writeFile(join(folder, "a.png"), png(1200, 1000));
    await rm(join(folder, "b.png"));
    await symlink(join(folder, "elsewhere.png"), join(folder, "b.png"));
    const fresh = fakeImages({});
    assert.deepEqual(
      await createScreenshotThumbnails({ platform: "darwin", images: fresh.images })([a, b]),
      ["", ""],
    );
    assert.deepEqual(fresh.calls, []);
    // Only the most recently shown are kept.
    const few = fakeImages({});
    const small = createScreenshotThumbnails({ platform: "darwin", images: few.images, keep: 1 });
    listed = await listScreenshots(folder);
    await small(listed);
    await small(listed);
    assert.equal(few.calls.length, 2 * listed.length - 1);
  } finally {
    await rm(folder, { recursive: true, force: true });
  }
});
