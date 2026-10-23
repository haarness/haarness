import test from "node:test";
import assert from "node:assert/strict";
import {
  PROJECT_IMAGE_LIMITS,
  changesProject,
  fileImage,
  forgetProjectImages,
  holdProjectImages,
  imageType,
  onProjectImageSettled,
  projectChanged,
  projectImagePath,
  relabelProjectImages,
  requestProjectImage,
  shownProjectImage,
  sniffImage,
  type ShownImage,
} from "../src/renderer/project-images";
import { madeHere, replyAttribute } from "../src/renderer/remote-images";

const root = "/Users/me/work/app";
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

test("a reply's own markup cannot dress as a project image", () => {
  assert.equal(replyAttribute("data-project-path", "../../etc/a.png"), undefined);
  assert.equal(replyAttribute("DATA-PROJECT-ALT", "x"), undefined);
  assert.equal(replyAttribute("class", "project-image"), undefined);
  assert.equal(replyAttribute("class", "wide project-image-loaded Project-Image-note"), "wide");
});

test("a turn's end, the file API and a tool that writes say the project may have changed", () => {
  assert.equal(changesProject({ type: "turn.finished" }), true);
  assert.equal(changesProject({ type: "files.updated" }), true);
  const result = (name: string, is_error = false) => ({
    type: "item.created",
    payload: { data: { type: "tool_result", name, is_error } },
  });
  for (const name of ["write", "edit", "apply_patch", "bash", "shell"]) assert.equal(changesProject(result(name)), true, name);
  for (const name of ["read", "grep", "webfetch"]) assert.equal(changesProject(result(name)), false, name);
  assert.equal(changesProject(result("write", true)), false);
  assert.equal(changesProject({ type: "turn.text_delta" }), false);
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

test("an image is read once for its project, shown to every reply, and read again only after a change", async () => {
  forgetProjectImages();
  const files = { "a.png": png, "b.png": jpeg } as Record<string, File | Uint8Array>;
  const source = reader(files);
  const { settled, off } = heard();
  const live = new AbortController();
  requestProjectImage("project", "a.png", source.read, live.signal);
  requestProjectImage("project", "a.png", source.read, live.signal);
  await settle();
  assert.deepEqual(source.calls, ["a.png"]);
  const shown = { data: `data:image/png;base64,${base64(png)}` };
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

  // A file that changed is shown anew; one that cannot be read again keeps its picture.
  files["a.png"] = jpeg;
  projectChanged();
  requestProjectImage("project", "a.png", source.read, live.signal);
  await settle();
  assert.deepEqual(settled.at(-1), ["project", "a.png", { data: `data:image/jpeg;base64,${base64(jpeg)}` }]);
  files["a.png"] = new Error("This file no longer exists");
  projectChanged("project");
  requestProjectImage("project", "a.png", source.read, live.signal);
  await settle();
  assert.deepEqual(shownProjectImage("project", "a.png"), { data: `data:image/jpeg;base64,${base64(jpeg)}` });
  off();
});

test("a missing file, a refusal, text, bytes the window cannot draw or a file too large say why", async () => {
  forgetProjectImages();
  const source = reader({
    "missing.png": new Error("Error invoking remote method 'files': Error: This file no longer exists"),
    "escape.png": new Error("Error invoking remote method 'files': Error: Path escapes the workspace"),
    "text.png": { content: "hello", encoding: "utf8" },
    "fake.png": ascii("GIF89a not really"),
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
