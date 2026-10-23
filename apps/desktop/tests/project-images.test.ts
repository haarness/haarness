import test, { afterEach } from "node:test";
import assert from "node:assert/strict";
import {
  PROJECT_IMAGE_LIMITS,
  askOnce,
  fileImage,
  forgetProjectImages,
  holdProjectImages,
  imageHeader,
  imageType,
  applyProjectEvent,
  inViewedTree,
  localProject,
  onProjectImageSettled,
  projectChanged,
  projectEvent,
  projectFileWritten,
  projectImagePath,
  projectRoots,
  readToolImage,
  relabelProjectImages,
  requestProjectImage,
  shownProjectImage,
  sniffImage,
  windowFocused,
  windowsPath,
  type ShownImage,
} from "../src/renderer/project-images";
import { madeHere, replyAttribute } from "../src/renderer/remote-images";

const root = "/Users/me/work/app";
// Reads waiting for the recheck time end with each test.
afterEach(() => forgetProjectImages());
const ascii = (text: string) => Uint8Array.from(text, (char) => char.charCodeAt(0));
const join = (...parts: Uint8Array[]) => Uint8Array.from(parts.flatMap((part) => [...part]));
const base64 = (bytes: Uint8Array) => Buffer.from(bytes).toString("base64");

const png = Uint8Array.of(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52);
const jpeg = Uint8Array.of(0xff, 0xd8, 0xff, 0xe0, 0, 16, 0x4a, 0x46);
const gif = ascii("GIF89a\u0001\u0000\u0001\u0000");
const webp = ascii("RIFF\u0010\u0000\u0000\u0000WEBPVP8 ");
const avif = join(Uint8Array.of(0, 0, 0, 0x1c), ascii("ftypmif1"), Uint8Array.of(0, 0, 0, 0), ascii("mif1avif"));
const bmp = ascii("BM\u0000\u0000\u0000\u0000");
const ico = Uint8Array.of(0, 0, 1, 0, 1, 0, 16, 16);

test("a path in the project names its file there; anything else is outside or not a path", () => {
  for (const [src, path] of [
    ["out/chart.png", "out/chart.png"],
    ["./out/chart.png", "out/chart.png"],
    ["out/../chart.png", "chart.png"],
    ["out\\win\\chart.png", "out/win/chart.png"],
    [" out/chart.png\n", "out/chart.png"],
    ["out/chart.png?v=2#top", "out/chart.png"],
    // Markdown writes spaces and accents percent-encoded.
    ["out/My%20chart%20caf%C3%A9.png", "out/My chart café.png"],
    ["/Users/me/work/app/out/chart.png", "out/chart.png"],
    ["file:///Users/me/work/app/out/chart.png", "out/chart.png"],
    ["file://localhost/Users/me/work/app/out/a%20b.png", "out/a b.png"],
    ["out/chart.JPG", "out/chart.JPG"],
  ] as const)
    assert.deepEqual(projectImagePath(src, root), { path }, src);

  for (const src of [
    "../secret.png",
    "out/../../secret.png",
    "/etc/chart.png",
    "/Users/me/work/app2/chart.png",
    "/Users/me/work/apps.png",
    "~/chart.png",
    "file:///etc/chart.png",
    "file://fileserver/share/chart.png",
    "C:/proj/chart.png",
  ])
    assert.deepEqual(projectImagePath(src, root), { refused: "outside" }, src);
  // Without a project nothing is read.
  assert.deepEqual(projectImagePath("chart.png", ""), { refused: "outside" });

  // Not a raster image, or spelled with a control character: never read.
  for (const src of ["diagram.svg", "notes.txt", "chart", "chart.png.txt", "out/", "/Users/me/work/app", "a%00.png", "a%0A.png"])
    assert.deepEqual(projectImagePath(src, root), { refused: "failed" }, src);

  // Not a path at all: the sanitizer and the remote placeholder decide.
  for (const src of [
    "https://example.com/a.png",
    "HTTP://example.com/a.png",
    "data:image/png;base64,AAAA",
    "blob:https://x/1",
    "//fileserver/share/a.png",
    "\\\\fileserver\\share\\a.png",
    "/\t/fileserver/a.png",
    "#fragment",
    "javascript:alert(1)",
    "cid:part",
    "",
    null,
    undefined,
  ])
    assert.equal(projectImagePath(src, root), undefined, String(src));
});

test("a Windows project compares paths without case, and reads its drive paths", () => {
  const windows = "C:\\Users\\me\\app";
  assert.deepEqual(projectImagePath("C:\\Users\\me\\app\\out\\a.png", windows), { path: "out/a.png" });
  assert.deepEqual(projectImagePath("c:/users/ME/app/a.png", windows), { path: "a.png" });
  assert.deepEqual(projectImagePath("file:///C:/Users/me/app/a.png", windows), { path: "a.png" });
  assert.deepEqual(projectImagePath("D:/app/a.png", windows), { refused: "outside" });
});

test("only raster types are read, and only bytes that start as one are shown", () => {
  for (const [path, type] of [
    ["a.png", "image/png"],
    ["a.JPG", "image/jpeg"],
    ["a.jpeg", "image/jpeg"],
    ["a.gif", "image/gif"],
    ["a.webp", "image/webp"],
    ["a.avif", "image/avif"],
    ["a.bmp", "image/bmp"],
    ["a.ico", "image/x-icon"],
  ] as const)
    assert.equal(imageType(path), type, path);
  for (const path of ["a.svg", "a.tiff", "a.heic", "a.png.txt", "png"]) assert.equal(imageType(path), undefined, path);

  for (const [bytes, type] of [
    [png, "image/png"],
    [jpeg, "image/jpeg"],
    [gif, "image/gif"],
    [webp, "image/webp"],
    [avif, "image/avif"],
    [bmp, "image/bmp"],
    [ico, "image/x-icon"],
  ] as const)
    assert.equal(sniffImage(bytes), type);
  for (const bytes of [
    ascii("<svg xmlns='http://www.w3.org/2000/svg'/>"),
    ascii("RIFF\u0010\u0000\u0000\u0000WAVEfmt "),
    join(Uint8Array.of(0, 0, 0, 0x14), ascii("ftypisom"), Uint8Array.of(0, 0, 0, 0), ascii("isom")),
    ascii("hello"),
    new Uint8Array(),
  ])
    assert.equal(sniffImage(bytes), undefined);

  // The server's answer becomes a data URL of the type its bytes are, whatever the name says.
  assert.equal(fileImage({ content: base64(jpeg), encoding: "base64" }), `data:image/jpeg;base64,${base64(jpeg)}`);
  assert.equal(fileImage({ content: base64(png), encoding: "base64" }), `data:image/png;base64,${base64(png)}`);
  for (const file of [
    { content: "<svg/>", encoding: "utf8" },
    { content: base64(ascii("<svg/>")), encoding: "base64" },
    { content: "", encoding: "base64" },
    { content: "%%%%", encoding: "base64" },
    {},
    undefined,
  ])
    assert.equal(fileImage(file), undefined, JSON.stringify(file));
});

const be32 = (n: number) => [(n >>> 24) & 255, (n >>> 16) & 255, (n >>> 8) & 255, n & 255];
const le16 = (n: number) => [n & 255, (n >>> 8) & 255];
const le24 = (n: number) => [n & 255, (n >>> 8) & 255, (n >>> 16) & 255];
const le32 = (n: number) => [...le16(n & 0xffff), ...le16((n >>> 16) & 0xffff)];
const bytes = (...parts: (Uint8Array | number[] | string)[]) =>
  join(...parts.map((part) => (typeof part === "string" ? ascii(part) : Uint8Array.from(part))));
const pngSized = (width: number, height: number) => bytes(png, be32(width), be32(height), [8, 2, 0, 0, 0]);
// A JPEG whose frame comes after two large application segments, past what one slice decodes.
const jpegSized = (width: number, height: number) =>
  bytes(
    [0xff, 0xd8, 0xff, 0xe0, 0, 16],
    "JFIF\0",
    new Array(9).fill(0),
    [0xff, 0xe1, ...be32(60_000).slice(2)],
    new Array(60_000 - 2).fill(7),
    [0xff, 0xe2, ...be32(60_000).slice(2)],
    new Array(60_000 - 2).fill(0xff),
    [0xff, 0xc2, 0, 17, 8, ...be32(height).slice(2), ...be32(width).slice(2), 3],
    new Array(9).fill(0),
  );

test("an image's size in pixels comes from its header, without decoding it", () => {
  const riff = (chunk: string, ...rest: (number[] | Uint8Array)[]) => bytes("RIFF", le32(100), "WEBP", chunk, le32(80), ...rest);
  for (const [name, file, width, height] of [
    ["png", pngSized(4000, 3000), 4000, 3000],
    ["gif", bytes("GIF89a", le16(640), le16(480)), 640, 480],
    ["bmp", bytes("BM", new Array(12).fill(0), le32(40), le32(800), le32(-600)), 800, 600],
    ["bmp core", bytes("BM", new Array(12).fill(0), le32(12), le16(32), le16(16)), 32, 16],
    ["webp lossy", riff("VP8 ", [0, 0, 0, 0x9d, 0x01, 0x2a], le16(1920), le16(1080)), 1920, 1080],
    ["webp lossless", riff("VP8L", [0x2f, 0x7f, 0x80, 0x39, 0x01]), 128, 1255],
    ["webp extended", riff("VP8X", [0, 0, 0, 0], le24(8191), le24(4095)), 8192, 4096],
    ["jpeg", jpegSized(6000, 4000), 6000, 4000],
    ["ico", bytes([0, 0, 1, 0, 1, 0, 0, 48]), 256, 48],
    [
      "avif",
      bytes(be32(0x1c), "ftypavif", be32(0), "avifmif1", be32(20), "ispe", be32(0), be32(5000), be32(4000)),
      5000,
      4000,
    ],
  ] as const)
    assert.deepEqual(imageHeader(base64(file)), { type: sniffImage(file), width, height }, name);
  // An AVIF whose size is not near its start is only typed here; the window reads its size before decoding it.
  assert.deepEqual(imageHeader(base64(avif)), { type: "image/avif" });
  assert.equal(imageHeader(base64(ascii("<svg/>"))), undefined);
});

test("an image of more than 50 megapixels is too large to show and is never decoded", async () => {
  forgetProjectImages();
  const decoded: string[] = [];
  const decode = async (data: string) => (decoded.push(data), true);
  const source = reader({ "huge.png": pngSized(10_000, 10_000), "huge.jpg": jpegSized(9000, 6000), "big.png": pngSized(7000, 7000) });
  const live = new AbortController();
  for (const path of ["huge.png", "huge.jpg", "big.png"]) requestProjectImage("project", path, source.read, live.signal, decode);
  await settle();
  assert.deepEqual(shownProjectImage("project", "huge.png"), { note: "large" });
  assert.deepEqual(shownProjectImage("project", "huge.jpg"), { note: "large" });
  assert.ok(shownProjectImage("project", "big.png")?.data);
  assert.equal(decoded.length, 1);
});

test("a read's preview is the image its result carries, or the project file it read", () => {
  const done = (attachment: string, mime = "image/png") => ({
    is_error: false,
    metadata: { attachments: [attachment], mime, path: `${root}/shot.png` },
  });
  const carried = `data:image/png;name=shot.png;base64,${base64(png)}`;
  assert.deepEqual(readToolImage("read", done(carried), root), { data: `data:image/png;base64,${base64(png)}` });
  assert.deepEqual(readToolImage("read", done(`${root}/out/shot.png`), root), { path: "out/shot.png" });
  // The size the read saw, to tell the file as it was read from one changed since.
  assert.deepEqual(
    readToolImage("read", { is_error: false, metadata: { attachments: [`${root}/a.png`], mime: "image/png", size: 1234 } }, root),
    { path: "a.png", size: 1234 },
  );
  // Outside the project, an image the result does not carry is not shown.
  assert.equal(readToolImage("read", done("/tmp/shot.png"), root), undefined);
  // An SVG, bytes that are no image, another type or a failed read keep the row as it is.
  assert.equal(readToolImage("read", done(`data:image/svg+xml;base64,${base64(ascii("<svg/>"))}`, "image/svg+xml"), root), undefined);
  assert.equal(readToolImage("read", done(`data:image/png;base64,${base64(ascii("not a png"))}`), root), undefined);
  assert.equal(readToolImage("read", done(`${root}/a.pdf`, "application/pdf"), root), undefined);
  assert.equal(readToolImage("read", { ...done(carried), is_error: true }, root), undefined);
  assert.equal(readToolImage("read", { is_error: false, metadata: { mime: "image/png" } }, root), undefined);
  assert.equal(readToolImage("read", undefined, root), undefined);
  assert.equal(readToolImage("bash", done(carried), root), undefined);
});

test("a reply's own markup cannot dress as a project image or as one that opens larger", () => {
  assert.equal(replyAttribute("data-project-path", "../../etc/a.png"), undefined);
  assert.equal(replyAttribute("DATA-PROJECT-ALT", "x"), undefined);
  assert.equal(replyAttribute("data-image-open", ""), undefined);
  assert.equal(replyAttribute("class", "project-image"), undefined);
  assert.equal(replyAttribute("class", "wide project-image-loaded Project-Image-note"), "wide");
  assert.equal(replyAttribute("data-image-opener", "x"), "x");
});

test("what each server event means for the images: the viewed chat's turn at once, another's once per turn, a write at its result", () => {
  const threads = [{ id: "child", parentId: "mine" }, { id: "mine" }, { id: "other" }];
  const viewing = (selected: string) => (thread?: string | null, root?: string | null) =>
    inViewedTree(selected, threads, thread, root);
  const viewed = viewing("mine");
  const result = (name: string, threadId = "mine", rootThreadId: string | null = threadId, is_error = false, call_id = "c0") => ({
    type: "item.created",
    threadId,
    rootThreadId,
    payload: { data: { type: "tool_result", name, is_error, call_id } },
  });
  assert.deepEqual(projectEvent({ type: "files.updated", payload: { path: "out/a.png" } }, viewed), { written: "out/a.png" });
  assert.equal(projectEvent({ type: "files.updated", payload: {} }, viewed), undefined);
  // The viewed chat's turn end reads its images again at once; another chat's, once per turn, after the wait.
  assert.deepEqual(projectEvent({ type: "turn.finished", threadId: "mine" }, viewed), { changed: true, now: true });
  // A subagent's turn end is a change of the tree, not the viewed chat's own: it waits.
  assert.deepEqual(projectEvent({ type: "turn.finished", threadId: "grandchild", rootThreadId: "mine" }, viewed), {
    changed: true,
    now: false,
  });
  assert.deepEqual(projectEvent({ type: "turn.finished", threadId: "other", rootThreadId: "other" }, viewed), {
    changed: true,
    now: false,
  });
  for (const name of ["write", "edit", "apply_patch", "bash", "shell"])
    assert.deepEqual(projectEvent(result(name), viewed), { changed: true }, name);
  // Its subagents' work is its own.
  assert.deepEqual(projectEvent(result("bash", "grandchild", "mine"), viewed), { changed: true });
  // Another chat's tools, however busy, change nothing but the write they finish; nor do reads or streamed text.
  assert.deepEqual(projectEvent(result("bash", "other"), viewed), { finished: "c0", thread: "other" });
  for (const name of ["read", "grep", "webfetch"])
    assert.deepEqual(projectEvent(result(name), viewed), { finished: "c0", thread: "mine" }, name);
  assert.deepEqual(projectEvent(result("write", "mine", "mine", true), viewed), { finished: "c0", thread: "mine", failed: true });
  assert.equal(projectEvent({ type: "turn.text_delta", threadId: "mine" }, viewed), undefined);
  // A write or edit that names an image in the project is writing it, until its result.
  const call = (name: string, args: Record<string, unknown>, threadId = "mine", call_id = "c1") => ({
    type: "item.created",
    threadId,
    rootThreadId: threadId,
    payload: { data: { type: "tool_call", name, call_id, arguments: args } },
  });
  assert.deepEqual(projectEvent(call("write", { path: "out/a.png" }), viewed, root), { writing: "out/a.png", call: "c1", thread: "mine" });
  assert.deepEqual(projectEvent(call("functions.edit", { filePath: `${root}/out/b.PNG` }), viewed, root), {
    writing: "out/b.PNG",
    call: "c1",
    thread: "mine",
  });
  // On another chat too: the file is the same.
  assert.deepEqual(projectEvent(call("write", { path: "out/a.png" }, "other"), viewed, root), {
    writing: "out/a.png",
    call: "c1",
    thread: "other",
  });
  assert.equal(projectEvent(call("write", { path: "src/a.ts" }), viewed, root), undefined);
  assert.equal(projectEvent(call("write", { path: "/etc/a.png" }), viewed, root), undefined);
  assert.equal(projectEvent(call("read", { path: "out/a.png" }), viewed, root), undefined);
  // Viewing a child chat, its whole tree counts.
  assert.deepEqual(projectEvent(result("bash", "mine", "mine"), viewing("child")), { changed: true });
  assert.deepEqual(projectEvent(result("bash", "other"), viewing("child")), { finished: "c0", thread: "other" });
  assert.equal(inViewedTree(undefined, threads, "mine", "mine"), undefined);
  assert.equal(inViewedTree("child", threads, "child", "mine"), "self");
  assert.equal(inViewedTree("child", threads, "mine", "mine"), "tree");
});

test("a project opened through a link, or by its Windows long path, holds the paths under its real folder", () => {
  assert.deepEqual(projectRoots({ workspace: "/tmp/proj", info: { workspace: "/private/tmp/proj" } }), ["/tmp/proj", "/private/tmp/proj"]);
  assert.deepEqual(projectRoots({ workspace: "/work/app", info: { workspace: "/work/app" } }), ["/work/app"]);
  const linked = projectRoots({ workspace: "/tmp/proj", info: { workspace: "/private/tmp/proj" } });
  for (const src of ["/private/tmp/proj/out/a.png", "/tmp/proj/out/a.png", "out/a.png", "file:///private/tmp/proj/out/a.png"])
    assert.deepEqual(projectImagePath(src, linked), { path: "out/a.png" }, src);
  assert.deepEqual(projectImagePath("/private/tmp/other/a.png", linked), { refused: "outside" });
  // A read's canonical path on Windows carries \\?\; a share's \\?\UNC\ stays a network path.
  const windows = projectRoots({ workspace: "C:\\Users\\me\\app", info: { workspace: "\\\\?\\C:\\Users\\me\\app" } });
  assert.deepEqual(windows, ["C:\\Users\\me\\app"]);
  assert.equal(windowsPath("\\\\?\\C:\\x"), "C:\\x");
  assert.equal(windowsPath("\\\\?\\UNC\\host\\share\\x"), "\\\\?\\UNC\\host\\share\\x");
  assert.deepEqual(projectImagePath("\\\\?\\C:\\Users\\me\\app\\shot.png", windows), { path: "shot.png" });
  assert.equal(projectImagePath("\\\\?\\UNC\\host\\share\\shot.png", windows), undefined);
  assert.deepEqual(
    readToolImage("read", { is_error: false, metadata: { attachments: ["/private/tmp/proj/shot.png"], mime: "image/png", size: 9 } }, linked),
    { path: "shot.png", size: 9 },
  );
});

test("a reply asks for each of its images once, and for at most its share of paths", () => {
  const asked = new Set<string>();
  const requested: string[] = [];
  const paths = Array.from({ length: PROJECT_IMAGE_LIMITS.paths + 2 }, (_, index) => `${index}.png`);
  for (let patch = 0; patch < 5; patch++) {
    const over = askOnce(paths, asked, (path) => requested.push(path));
    assert.deepEqual([...over], paths.slice(PROJECT_IMAGE_LIMITS.paths));
  }
  assert.deepEqual(requested, paths.slice(0, PROJECT_IMAGE_LIMITS.paths));
});

type File = { content?: string; encoding?: string } | Error | undefined;

/** A reader in place of the server's file API: it records each path and answers from the table, or waits. */
function reader(files: Record<string, File | Uint8Array> = {}, hold = false) {
  const calls: string[] = [];
  const waiting: (() => void)[] = [];
  return {
    calls,
    release: () => waiting.shift()?.(),
    read: async (path: string) => {
      calls.push(path);
      if (hold) await new Promise<void>((done) => waiting.push(done));
      const file = files[path];
      if (file instanceof Error) throw file;
      if (file instanceof Uint8Array) return { content: base64(file), encoding: "base64" };
      return file;
    },
  };
}

const settle = async () => {
  for (let index = 0; index < 6; index++) await new Promise((done) => setTimeout(done, 0));
};

function heard() {
  const settled: [string, string, ShownImage][] = [];
  const off = onProjectImageSettled((scope, path, shown) => settled.push([scope, path, shown]));
  return { settled, off };
}

/** Runs `body` with a remembered image read again at once after a change. */
async function rechecking(body: () => Promise<void>) {
  const recheck = PROJECT_IMAGE_LIMITS.recheck;
  PROJECT_IMAGE_LIMITS.recheck = 0;
  try {
    await body();
  } finally {
    PROJECT_IMAGE_LIMITS.recheck = recheck;
  }
}

test("an image is read once for its project, shown to every reply, and read again only after a change", () => rechecking(async () => {
  forgetProjectImages();
  const files = { "a.png": png, "b.png": jpeg } as Record<string, File | Uint8Array>;
  const source = reader(files);
  const { settled, off } = heard();
  const live = new AbortController();
  requestProjectImage("project", "a.png", source.read, live.signal);
  requestProjectImage("project", "a.png", source.read, live.signal);
  await settle();
  assert.deepEqual(source.calls, ["a.png"]);
  const shown = { data: `data:image/png;base64,${base64(png)}`, size: png.length };
  assert.deepEqual(shownProjectImage("project", "a.png"), shown);
  assert.deepEqual(settled, [["project", "a.png", shown]]);
  // Another project reads its own file of that name.
  assert.equal(shownProjectImage("other", "a.png"), undefined);

  // Drawn again: no read.
  requestProjectImage("project", "a.png", source.read, live.signal);
  await settle();
  assert.equal(source.calls.length, 1);

  // After a change the image keeps showing, is read again, and nothing changes when the file did not.
  projectChanged("project");
  assert.deepEqual(shownProjectImage("project", "a.png"), shown);
  requestProjectImage("project", "a.png", source.read, live.signal);
  await settle();
  assert.deepEqual([source.calls.length, settled.length], [2, 1]);

  // A file that changed is shown anew, with its size; one that is gone says so from then on.
  files["a.png"] = jpeg;
  projectChanged("project");
  requestProjectImage("project", "a.png", source.read, live.signal);
  await settle();
  assert.deepEqual(settled.at(-1), ["project", "a.png", { data: `data:image/jpeg;base64,${base64(jpeg)}`, size: jpeg.length }]);
  files["a.png"] = new Error("This file no longer exists");
  projectChanged("project");
  requestProjectImage("project", "a.png", source.read, live.signal);
  await settle();
  assert.deepEqual(shownProjectImage("project", "a.png"), { note: "failed" });
  assert.deepEqual(settled.at(-1), ["project", "a.png", { note: "failed" }]);
  off();
}));

/** A server event of the project, as the window hears it, with "mine" the chat being viewed. */
const hear = (value: Parameters<typeof projectEvent>[0], viewing = "mine") =>
  applyProjectEvent("project", value, (thread, root) => inViewedTree(viewing, [], thread, root), root);
const toolResult = (threadId: string, name = "bash", call_id = "c0", is_error = false) => ({
  type: "item.created",
  threadId,
  rootThreadId: threadId,
  payload: { data: { type: "tool_result", name, call_id, is_error } },
});
const wait = (ms: number) => new Promise((done) => setTimeout(done, ms));

test("a live reply reads each image once while other chats, its own tools and the window keep busy", async () => {
  forgetProjectImages();
  const source = reader({ "a.png": png, "b.png": jpeg });
  // A reply streams in 60 pieces; each is drawn again, as Markdown.tsx does, while
  // another chat and a subagent of this one run commands and the window comes forward.
  const live = new AbortController();
  const asked = new Set<string>();
  for (let piece = 0; piece < 60; piece++) {
    askOnce(piece < 30 ? ["a.png"] : ["a.png", "b.png"], asked, (path) =>
      requestProjectImage("project", path, source.read, live.signal),
    );
    hear(toolResult("other"));
    hear({ ...toolResult("sub"), rootThreadId: "mine" });
    windowFocused();
    await settle();
  }
  assert.deepEqual(source.calls, ["a.png", "b.png"]);
  // Another chat's tools, however many, leave its images alone.
  for (let index = 0; index < 50; index++) hear(toolResult("other"));
  requestProjectImage("project", "a.png", source.read, live.signal);
  await settle();
  assert.deepEqual(source.calls, ["a.png", "b.png"]);
  forgetProjectImages();
});

test("drawn again within the recheck time after a change, an image is read once when it ends, and every reply takes it", async () => {
  forgetProjectImages();
  const recheck = PROJECT_IMAGE_LIMITS.recheck;
  PROJECT_IMAGE_LIMITS.recheck = 80;
  try {
    const files = { "a.png": png } as Record<string, File | Uint8Array>;
    const source = reader(files);
    const { settled, off } = heard();
    const replies = Array.from({ length: 6 }, () => new AbortController());
    requestProjectImage("project", "a.png", source.read, replies[0].signal);
    await settle();
    // The agent's command makes a new chart, and five replies draw it before the time is up.
    files["a.png"] = jpeg;
    hear(toolResult("mine"));
    for (const reply of replies.slice(1)) requestProjectImage("project", "a.png", source.read, reply.signal);
    await settle();
    assert.deepEqual(source.calls, ["a.png"]);
    await wait(120);
    await settle();
    assert.deepEqual(source.calls, ["a.png", "a.png"]);
    assert.deepEqual(settled.at(-1), ["project", "a.png", { data: `data:image/jpeg;base64,${base64(jpeg)}`, size: jpeg.length }]);
    // Replies that have all gone leave nothing to read.
    for (const reply of replies) reply.abort();
    hear(toolResult("mine"));
    const gone = new AbortController();
    requestProjectImage("project", "a.png", source.read, gone.signal);
    gone.abort();
    await wait(120);
    await settle();
    assert.equal(source.calls.length, 2);
    off();
  } finally {
    PROJECT_IMAGE_LIMITS.recheck = recheck;
    forgetProjectImages();
  }
});

test("a fast turn: the chart its command remade shows once the turn ends, not twenty seconds later", async () => {
  forgetProjectImages();
  const files = { "chart.png": png } as Record<string, File | Uint8Array>;
  const source = reader(files);
  const { settled, off } = heard();
  requestProjectImage("project", "chart.png", source.read, new AbortController().signal);
  await settle();
  // The command remakes it and the live reply draws it at once: the old one shows for now.
  files["chart.png"] = jpeg;
  hear(toolResult("mine"));
  const live = new AbortController();
  requestProjectImage("project", "chart.png", source.read, live.signal);
  await settle();
  assert.equal(source.calls.length, 1);
  // The viewed chat's turn ends: read at once, and the reply on screen takes the new chart.
  hear({ type: "turn.finished", threadId: "mine", rootThreadId: "mine" });
  await settle();
  assert.equal(source.calls.length, 2);
  assert.deepEqual(settled.at(-1)?.[2], { data: `data:image/jpeg;base64,${base64(jpeg)}`, size: jpeg.length });
  // A reply drawn after it does not read it again.
  requestProjectImage("project", "chart.png", source.read, new AbortController().signal);
  await settle();
  assert.equal(source.calls.length, 2);
  off();
  forgetProjectImages();
});

test("an earlier reply on screen takes the chart its chat remade once the turn ends, though the last reply only says Done", async () => {
  const files = { "chart.png": png, "other.png": png } as Record<string, File | Uint8Array>;
  const source = reader(files);
  const { settled, off } = heard();
  // Reply 1 shows the chart and stays on screen; reply 2 shows no image.
  const reply = new AbortController();
  requestProjectImage("project", "chart.png", source.read, reply.signal);
  await settle();
  files["chart.png"] = jpeg;
  hear({ type: "turn.finished", threadId: "mine", rootThreadId: "mine" });
  await settle();
  assert.deepEqual(source.calls, ["chart.png", "chart.png"]);
  assert.deepEqual(settled.at(-1), ["project", "chart.png", { data: `data:image/jpeg;base64,${base64(jpeg)}`, size: jpeg.length }]);
  // An image no reply shows any more is not read for it.
  reply.abort();
  hear({ type: "turn.finished", threadId: "mine", rootThreadId: "mine" });
  await settle();
  assert.equal(source.calls.length, 2);
  // The file API writing a file on screen reads it at once for the replies showing it.
  const other = new AbortController();
  requestProjectImage("project", "other.png", source.read, other.signal);
  await settle();
  files["other.png"] = jpeg;
  hear({ type: "files.updated", payload: { path: "other.png" } });
  await settle();
  assert.deepEqual(source.calls, ["chart.png", "chart.png", "other.png", "other.png"]);
  off();
});

test("a first read that got no answer is tried again for the reply still showing it", async () => {
  const recheck = PROJECT_IMAGE_LIMITS.recheck;
  PROJECT_IMAGE_LIMITS.recheck = 60;
  try {
    const files = { "a.png": new Error("Error invoking remote method 'files': TypeError: fetch failed") } as Record<
      string,
      File | Uint8Array | Error
    >;
    const source = reader(files);
    const { settled, off } = heard();
    requestProjectImage("project", "a.png", source.read, new AbortController().signal);
    await settle();
    assert.deepEqual(shownProjectImage("project", "a.png"), { note: "failed" });
    files["a.png"] = png;
    await wait(100);
    await settle();
    assert.equal(source.calls.length, 2);
    assert.deepEqual(settled.at(-1)?.[2], { data: `data:image/png;base64,${base64(png)}`, size: png.length });
    off();
  } finally {
    PROJECT_IMAGE_LIMITS.recheck = recheck;
  }
});

test("a draw that joins a read already running gets the file as it is after a change made meanwhile", async () => {
  const files = { "a.png": png } as Record<string, File | Uint8Array>;
  const source = reader(files, true);
  const { settled, off } = heard();
  requestProjectImage("project", "a.png", source.read, new AbortController().signal);
  await settle();
  // While that first read runs, the file is written and another reply draws it.
  hear({ type: "files.updated", payload: { path: "a.png" } });
  requestProjectImage("project", "a.png", source.read, new AbortController().signal);
  source.release();
  await settle();
  // The read that ran saw the file from before; it is read once more.
  files["a.png"] = jpeg;
  assert.equal(source.calls.length, 2);
  source.release();
  await settle();
  assert.equal(source.calls.length, 2);
  assert.deepEqual(settled.at(-1)?.[2], { data: `data:image/jpeg;base64,${base64(jpeg)}`, size: jpeg.length });
  off();
});

test("many subagent turn ends with many images on screen read each image once, after the wait", async () => {
  const recheck = PROJECT_IMAGE_LIMITS.recheck;
  PROJECT_IMAGE_LIMITS.recheck = 80;
  try {
    const paths = Array.from({ length: 12 }, (_, index) => `${index}.png`);
    const source = reader(Object.fromEntries(paths.map((path) => [path, png])));
    const reply = new AbortController();
    for (const path of paths) requestProjectImage("project", path, source.read, reply.signal);
    await settle();
    await wait(90);
    for (let index = 0; index < 60; index++) {
      hear({ type: "turn.finished", threadId: `sub-${index}`, rootThreadId: "mine" });
      hear(toolResult("other"));
    }
    await settle();
    // Past the wait already: each image is read once now, not once per event.
    assert.equal(source.calls.length, paths.length * 2);
    for (let index = 0; index < 60; index++) hear({ type: "turn.finished", threadId: `sub-${index}`, rootThreadId: "mine" });
    await settle();
    assert.equal(source.calls.length, paths.length * 2);
    await wait(120);
    await settle();
    assert.equal(source.calls.length, paths.length * 3);
  } finally {
    PROJECT_IMAGE_LIMITS.recheck = recheck;
  }
});

test("a chat switch: a chart another chat remade shows when its chat is drawn again", async () => {
  forgetProjectImages();
  const recheck = PROJECT_IMAGE_LIMITS.recheck;
  PROJECT_IMAGE_LIMITS.recheck = 80;
  try {
    const files = { "chart.png": png } as Record<string, File | Uint8Array>;
    const source = reader(files);
    requestProjectImage("project", "chart.png", source.read, new AbortController().signal);
    await settle();
    // Chat A remakes the chart and ends its turn while B is viewed: a change, once per turn.
    files["chart.png"] = jpeg;
    for (let index = 0; index < 20; index++) hear(toolResult("A"), "B");
    hear({ type: "turn.finished", threadId: "A", rootThreadId: "A" }, "B");
    // Back in A, its reply is drawn again within the recheck time: read when it ends.
    requestProjectImage("project", "chart.png", source.read, new AbortController().signal);
    await settle();
    assert.equal(source.calls.length, 1);
    await wait(120);
    await settle();
    assert.equal(source.calls.length, 2);
    assert.deepEqual(shownProjectImage("project", "chart.png"), { data: `data:image/jpeg;base64,${base64(jpeg)}`, size: jpeg.length });
    // Past the recheck time it is read at the draw itself.
    files["chart.png"] = png;
    hear({ type: "turn.finished", threadId: "A", rootThreadId: "A" }, "B");
    await wait(100);
    requestProjectImage("project", "chart.png", source.read, new AbortController().signal);
    await settle();
    assert.equal(source.calls.length, 3);
  } finally {
    PROJECT_IMAGE_LIMITS.recheck = recheck;
    forgetProjectImages();
  }
});

test("a write tool marks its file once its result arrives, not at its call, so a draw in between reads nothing", async () => {
  forgetProjectImages();
  const files = { "out/a.png": png } as Record<string, File | Uint8Array>;
  const source = reader(files);
  const { settled, off } = heard();
  const live = new AbortController();
  requestProjectImage("project", "out/a.png", source.read, live.signal);
  await settle();
  const call = (call_id: string) =>
    hear({
      type: "item.created",
      threadId: "other",
      rootThreadId: "other",
      payload: { data: { type: "tool_call", name: "write", call_id, arguments: { path: `${root}/out/a.png` } } },
    });
  // The call waits for approval; a reply drawn meanwhile reads nothing.
  call("w1");
  requestProjectImage("project", "out/a.png", source.read, new AbortController().signal);
  await settle();
  assert.equal(source.calls.length, 1);
  // The write is done: its result marks the file, and the next draw reads it at once.
  files["out/a.png"] = jpeg;
  hear(toolResult("other", "write", "w1"));
  requestProjectImage("project", "out/a.png", source.read, new AbortController().signal);
  await settle();
  assert.equal(source.calls.length, 2);
  assert.deepEqual(settled.at(-1)?.[2], { data: `data:image/jpeg;base64,${base64(jpeg)}`, size: jpeg.length });
  // A write that failed marks nothing.
  call("w2");
  hear(toolResult("other", "write", "w2", true));
  requestProjectImage("project", "out/a.png", source.read, new AbortController().signal);
  await settle();
  assert.equal(source.calls.length, 2);
  off();
  forgetProjectImages();
});

test("a read that answers nothing keeps the picture and tries again; only an answer replaces it", async () => {
  forgetProjectImages();
  const recheck = PROJECT_IMAGE_LIMITS.recheck;
  PROJECT_IMAGE_LIMITS.recheck = 80;
  try {
    const files = { "a.png": png } as Record<string, File | Uint8Array | Error>;
    const source = reader(files);
    const { settled, off } = heard();
    requestProjectImage("project", "a.png", source.read, new AbortController().signal);
    await settle();
    const picture = shownProjectImage("project", "a.png");
    // The connection drops (or the server fails) while it is read again: the picture stays.
    files["a.png"] = new Error("Error invoking remote method 'files': TypeError: fetch failed");
    projectFileWritten("project", "a.png");
    requestProjectImage("project", "a.png", source.read, new AbortController().signal);
    await settle();
    assert.deepEqual(shownProjectImage("project", "a.png"), picture);
    assert.equal(settled.length, 1);
    // It is tried again once the recheck time is over, by a reply still showing it.
    files["a.png"] = jpeg;
    requestProjectImage("project", "a.png", source.read, new AbortController().signal);
    await wait(120);
    await settle();
    assert.equal(source.calls.length, 3);
    assert.deepEqual(shownProjectImage("project", "a.png"), { data: `data:image/jpeg;base64,${base64(jpeg)}`, size: jpeg.length });
    // An answer that the file is gone does replace it.
    files["a.png"] = new Error("Error invoking remote method 'files': Error: This file no longer exists");
    projectFileWritten("project", "a.png");
    requestProjectImage("project", "a.png", source.read, new AbortController().signal);
    await settle();
    assert.deepEqual(shownProjectImage("project", "a.png"), { note: "failed" });
    off();
  } finally {
    PROJECT_IMAGE_LIMITS.recheck = recheck;
    forgetProjectImages();
  }
});

test("the window coming to the front reads a project on this computer again, never a remote host's", () => rechecking(async () => {
  forgetProjectImages();
  const source = reader({ "a.png": png });
  const live = new AbortController();
  for (const scope of ["here", "remote"]) requestProjectImage(scope, "a.png", source.read, live.signal);
  await settle();
  assert.deepEqual(source.calls, ["a.png", "a.png"]);
  localProject("here");
  windowFocused();
  for (const scope of ["here", "remote"]) requestProjectImage(scope, "a.png", source.read, live.signal);
  await settle();
  assert.equal(source.calls.length, 3);
}));

test("a missing file, a refusal, text, bytes the window cannot draw or a file too large say why", async () => {
  forgetProjectImages();
  const source = reader({
    "missing.png": new Error("Error invoking remote method 'files': Error: This file no longer exists"),
    "escape.png": new Error("Error invoking remote method 'files': Error: Path escapes the workspace"),
    "text.png": { content: "hello", encoding: "utf8" },
    "fake.png": ascii("GIF89a\u0001\u0000\u0001\u0000 not really"),
    "huge.png": new Error("Error invoking remote method 'files': Error: File exceeds 10 MiB"),
  });
  const live = new AbortController();
  for (const path of ["missing.png", "escape.png", "text.png", "huge.png"])
    requestProjectImage("project", path, source.read, live.signal);
  requestProjectImage("project", "fake.png", source.read, live.signal, async () => false);
  await settle();
  assert.deepEqual(
    ["missing.png", "escape.png", "text.png", "fake.png", "huge.png"].map((path) => shownProjectImage("project", path)),
    [{ note: "failed" }, { note: "failed" }, { note: "failed" }, { note: "failed" }, { note: "large" }],
  );
  // Remembered: a reply drawn again does not ask again until the project changes.
  requestProjectImage("project", "missing.png", source.read, live.signal);
  await settle();
  assert.equal(source.calls.filter((path) => path === "missing.png").length, 1);
});

test("at most a few reads run at once across every reply", async () => {
  forgetProjectImages();
  const paths = Array.from({ length: PROJECT_IMAGE_LIMITS.parallel + 2 }, (_, index) => `${index}.png`);
  const source = reader(Object.fromEntries(paths.map((path) => [path, png])), true);
  const live = new AbortController();
  for (const path of paths) requestProjectImage("project", path, source.read, live.signal);
  await settle();
  assert.equal(source.calls.length, PROJECT_IMAGE_LIMITS.parallel);
  source.release();
  await settle();
  assert.equal(source.calls.length, PROJECT_IMAGE_LIMITS.parallel + 1);
  for (const _ of paths) {
    source.release();
    await settle();
  }
  assert.equal(source.calls.length, paths.length);
  assert.ok(paths.every((path) => shownProjectImage("project", path)?.data));
});

test("a read that never answers ends at its deadline, gives its slot back, and its late answer changes nothing", async () => {
  forgetProjectImages();
  const wait = PROJECT_IMAGE_LIMITS.wait;
  PROJECT_IMAGE_LIMITS.wait = 30;
  try {
    const stuck = reader({ "slow.png": png }, true);
    const live = new AbortController();
    const { settled, off } = heard();
    for (let index = 0; index < PROJECT_IMAGE_LIMITS.parallel; index++)
      requestProjectImage("project", `slow-${index}.png`, stuck.read, live.signal);
    const next = reader({ "next.png": png });
    requestProjectImage("project", "next.png", next.read, live.signal);
    await settle();
    assert.deepEqual(next.calls, []);
    await new Promise((done) => setTimeout(done, 60));
    await settle();
    // Every stuck read ended as unavailable and the waiting one ran.
    assert.deepEqual(shownProjectImage("project", "slow-0.png"), { note: "failed" });
    assert.deepEqual(next.calls, ["next.png"]);
    assert.ok(shownProjectImage("project", "next.png")?.data);
    const before = settled.length;
    for (let index = 0; index < PROJECT_IMAGE_LIMITS.parallel; index++) stuck.release();
    await settle();
    assert.deepEqual(shownProjectImage("project", "slow-0.png"), { note: "failed" });
    assert.equal(settled.length, before);
    off();
  } finally {
    PROJECT_IMAGE_LIMITS.wait = wait;
  }
});

test("a read still waiting when every reply that asked for it unmounts never starts", async () => {
  forgetProjectImages();
  const held = reader({}, true);
  const live = new AbortController();
  for (let index = 0; index < PROJECT_IMAGE_LIMITS.parallel; index++)
    requestProjectImage("project", `busy-${index}.png`, held.read, live.signal);
  const gone = new AbortController();
  const kept = new AbortController();
  const source = reader({ "a.png": png, "b.png": png });
  requestProjectImage("project", "a.png", source.read, gone.signal);
  requestProjectImage("project", "b.png", source.read, gone.signal);
  // Another reply showing b is still there.
  requestProjectImage("project", "b.png", source.read, kept.signal);
  gone.abort();
  // An unmounted reply asks for nothing.
  requestProjectImage("project", "c.png", source.read, gone.signal);
  await settle();
  for (let index = 0; index < PROJECT_IMAGE_LIMITS.parallel; index++) held.release();
  await settle();
  assert.deepEqual(source.calls, ["b.png"]);
  assert.equal(shownProjectImage("project", "a.png"), undefined);
  // Drawn again later, the image is read then.
  requestProjectImage("project", "a.png", source.read, kept.signal);
  await settle();
  assert.deepEqual(source.calls, ["b.png", "a.png"]);
  live.abort();
});

test("remembered images stay within their byte budget, oldest first", async () => {
  forgetProjectImages();
  const bytes = PROJECT_IMAGE_LIMITS.bytes;
  const one = `data:image/png;base64,${base64(png)}`.length;
  PROJECT_IMAGE_LIMITS.bytes = one * 2;
  try {
    const source = reader({ "1.png": png, "2.png": png, "3.png": png });
    const live = new AbortController();
    for (const path of ["1.png", "2.png", "3.png"]) {
      requestProjectImage("project", path, source.read, live.signal);
      await settle();
    }
    assert.equal(shownProjectImage("project", "1.png"), undefined);
    assert.ok(shownProjectImage("project", "2.png")?.data);
    assert.ok(shownProjectImage("project", "3.png")?.data);
  } finally {
    PROJECT_IMAGE_LIMITS.bytes = bytes;
  }
});

/** Just enough of the DOM for holdProjectImages, which these tests have no browser for. */
class Fake {
  className = "";
  dataset: Record<string, string> = {};
  children: Fake[] = [];
  textContent = "";
  alt = "";
  src = "";
  replacement?: Fake;
  constructor(
    readonly ownerDocument: { createElement: (tag: string) => Fake },
    readonly tag: string,
    readonly attributes: Record<string, string> = {},
  ) {}
  getAttribute(name: string) {
    return this.attributes[name] ?? null;
  }
  setAttribute(name: string, value: string) {
    this.attributes[name] = value;
  }
  append(...nodes: Fake[]) {
    this.children.push(...nodes);
  }
  replaceWith(node: Fake) {
    this.replacement = node;
  }
  querySelector(selector: string): Fake | null {
    return this.children.find((child) => `.${child.className}` === selector) ?? null;
  }
}
const fakeDocument = { createElement: (tag: string): Fake => new Fake(fakeDocument, tag) };
const labels = { unnamed: "Image", outside: "Outside this project", large: "Too large to show", failed: "Not available" };
const hold = (images: Fake[], shown: (path: string) => ShownImage | undefined, written?: Map<Fake, string>) =>
  holdProjectImages(
    { querySelectorAll: () => images } as unknown as ParentNode,
    root,
    (image) => written?.get(image as unknown as Fake) ?? image.getAttribute("src"),
    shown,
    labels,
  );

test("a reply's path image becomes the image its project showed, or a placeholder the app made", () => {
  const data = `data:image/png;base64,${base64(png)}`;
  const known = new Fake(fakeDocument, "img", { src: "out/a.png", alt: "Chart" });
  const waiting = new Fake(fakeDocument, "img", { src: "out/b.png" });
  const outside = new Fake(fakeDocument, "img", { src: "/etc/x.png", alt: "Secret" });
  const svg = new Fake(fakeDocument, "img", { src: "d.svg", alt: "Diagram" });
  const remote = new Fake(fakeDocument, "img", { src: "https://example.com/a.png" });
  // The sanitizer took a local file: URL off; the source as written still names the file.
  const file = new Fake(fakeDocument, "img", { alt: "File" });
  hold([known, waiting, outside, svg, remote, file], (path) => (path === "out/a.png" ? { data } : undefined), new Map([[file, `file://${root}/c.png`]]));

  const image = known.replacement!;
  assert.deepEqual([image.tag, image.className, image.dataset.projectPath, image.alt, image.src], [
    "img",
    "project-image-loaded",
    "out/a.png",
    "Chart",
    data,
  ]);
  assert.equal(madeHere(image as unknown as Node), true);

  const holder = waiting.replacement!;
  assert.deepEqual([holder.className, holder.dataset.projectPath, holder.children.map((child) => child.textContent)], [
    "project-image",
    "out/b.png",
    ["Image"],
  ]);
  assert.equal(madeHere(holder as unknown as Node), true);
  assert.equal(file.replacement!.dataset.projectPath, "c.png");

  // A path outside the project is never read: its placeholder has no path and says why, naming it.
  const refused = outside.replacement!;
  assert.equal(refused.dataset.projectPath, undefined);
  assert.deepEqual(refused.children.map((child) => child.textContent), ["Secret", "Outside this project"]);
  assert.equal(refused.getAttribute("title"), "/etc/x.png");
  assert.deepEqual(svg.replacement!.children.map((child) => child.textContent), ["Diagram", "Not available"]);
  assert.equal(svg.replacement!.dataset.projectPath, undefined);
  // A remote image waits for its own placeholder.
  assert.equal(remote.replacement, undefined);

  // A copy of a placeholder is not one, and the notes follow a language change.
  const forged = new Fake(fakeDocument, "span");
  forged.className = "project-image";
  forged.dataset.projectPath = "../../etc/passwd.png";
  assert.equal(madeHere(forged as unknown as Node), false);
  relabelProjectImages({ querySelectorAll: () => [holder, refused] } as unknown as ParentNode, {
    unnamed: "صورة",
    outside: "خارج هذا المشروع",
    large: "أكبر من أن تُعرض",
    failed: "غير متاح",
  });
  assert.equal(holder.children[0].textContent, "صورة");
  assert.deepEqual(refused.children.map((child) => child.textContent), ["Secret", "خارج هذا المشروع"]);
});

test("a remembered note shows on the placeholder at once", () => {
  const image = new Fake(fakeDocument, "img", { src: "big.png", alt: "Big" });
  hold([image], () => ({ note: "large" }));
  assert.deepEqual(image.replacement!.children.map((child) => child.textContent), ["Big", "Too large to show"]);
  // Still with its path, so a later change reads it again.
  assert.equal(image.replacement!.dataset.projectPath, "big.png");
});
