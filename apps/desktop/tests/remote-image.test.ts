import test from "node:test";
import assert from "node:assert/strict";
import { createServer, type IncomingHttpHeaders, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { fetchRemoteImage, imageAddress, imageType, privateAddress } from "../src/main/remote-image";
import {
  forgetImages,
  holdRemoteImages,
  imageLoading,
  loadableSources,
  loadedImage,
  loadImage,
  madeHere,
  onImageSettled,
  relabelImages,
  rememberImage,
  remoteImageHost,
  replyAttribute,
} from "../src/renderer/remote-images";

test("an image from another host is named by its host; anything that stays in the page is not", () => {
  assert.equal(remoteImageHost("https://evil.example/pixel.png?d=secret"), "evil.example");
  assert.equal(remoteImageHost(" HTTP://Images.Example:8080/a.png "), "images.example");
  assert.equal(remoteImageHost("https://[2606:4700::1111]/a.png"), "[2606:4700::1111]");
  for (const src of ["data:image/png;base64,AAAA", "blob:https://x/1", "images/a.png", "/a.png", "file:///etc/a.png", "", null, undefined])
    assert.equal(remoteImageHost(src), undefined, String(src));
});

test("loaded images are kept per chat, newest first, within a byte budget", () => {
  forgetImages();
  rememberImage("chat-a", "https://x/1.png", "data:1111", 12);
  assert.equal(loadedImage("chat-a", "https://x/1.png"), "data:1111");
  // Another chat has not loaded it.
  assert.equal(loadedImage("chat-b", "https://x/1.png"), undefined);
  rememberImage("chat-a", "https://x/2.png", "data:2222", 12);
  // Past the budget the oldest image goes; one larger than it is never kept.
  assert.equal(loadedImage("chat-a", "https://x/1.png"), undefined);
  assert.equal(loadedImage("chat-a", "https://x/2.png"), "data:2222");
  rememberImage("chat-a", "https://x/3.png", "data:far-too-large", 12);
  assert.equal(loadedImage("chat-a", "https://x/3.png"), undefined);
  forgetImages();
  assert.equal(loadedImage("chat-a", "https://x/2.png"), undefined);
});

/** Just enough of the DOM for holdRemoteImages, which these tests have no browser for. */
class Fake {
  className = "";
  dataset: Record<string, string> = {};
  children: Fake[] = [];
  textContent = "";
  type = "";
  title = "";
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
  append(...nodes: Fake[]) {
    this.children.push(...nodes);
  }
  replaceWith(node: Fake) {
    this.replacement = node;
  }
  disabled = false;
  querySelector(selector: string): Fake | null {
    return this.children.find((child) => `.${child.className}` === selector) ?? null;
  }
}
const fakeDocument = { createElement: (tag: string): Fake => new Fake(fakeDocument, tag) };
const labels = { load: (host: string) => `Load image from ${host}`, unnamed: "Image" };

test("only placeholders the app made count as placeholders, whatever a copy looks like", () => {
  const image = new Fake(fakeDocument, "img", { src: "https://github.com/a.png", alt: "Logo" });
  holdRemoteImages({ querySelectorAll: () => [image] } as unknown as ParentNode, () => undefined, labels);
  const holder = image.replacement!;
  const button = holder.children[1];
  assert.equal(holder.className, "remote-image");
  assert.equal(holder.dataset.remoteHost, "github.com");
  assert.equal(button.textContent, "Load image from github.com");
  assert.equal(madeHere(holder as unknown as Node), true);
  assert.equal(madeHere(button as unknown as Node), true);
  // The same classes and data written by anything else are not one.
  const forged = new Fake(fakeDocument, "span");
  forged.className = "remote-image";
  Object.assign(forged.dataset, { remoteHost: "github.com", remoteSrc: "https://evil.example/?d=SECRET" });
  assert.equal(madeHere(forged as unknown as Node), false);
  assert.equal(madeHere(holder.children[0] as unknown as Node), false);
  assert.equal(madeHere(undefined), false);
});

test("a fetch already running keeps its button off, and placeholders follow a language change", () => {
  const image = new Fake(fakeDocument, "img", { src: "https://images.example/a.png" });
  const blank = new Fake(fakeDocument, "img", { src: "https://images.example/b.png", alt: "Chart" });
  holdRemoteImages(
    { querySelectorAll: () => [image, blank] } as unknown as ParentNode,
    () => undefined,
    labels,
    (src) => src.endsWith("a.png"),
  );
  const [busy, idle] = [image.replacement!, blank.replacement!];
  assert.equal(busy.children[1].disabled, true);
  assert.equal(idle.children[1].disabled, false);
  const forged = new Fake(fakeDocument, "span");
  forged.className = "remote-image";
  relabelImages({ querySelectorAll: () => [busy, idle, forged] } as unknown as ParentNode, {
    load: (host) => `تحميل الصورة من ${host}`,
    unnamed: "صورة",
  });
  assert.equal(busy.children[0].textContent, "صورة");
  assert.equal(busy.children[1].textContent, "تحميل الصورة من images.example");
  // Alt text the reply wrote stays as written.
  assert.equal(idle.children[0].textContent, "Chart");
  assert.equal(forged.children.length, 0);
});

test("an image is fetched once at a time per chat, and again after a failure", async () => {
  forgetImages();
  const fetched: string[] = [];
  let finish!: (data: string) => void;
  let fail!: (error: Error) => void;
  const fetch = (src: string) => {
    fetched.push(src);
    return new Promise<string>((resolve, reject) => {
      finish = resolve;
      fail = reject;
    });
  };
  const first = loadImage("chat", "https://x/a.png", fetch);
  const second = loadImage("chat", "https://x/a.png", fetch);
  assert.equal(imageLoading("chat", "https://x/a.png"), true);
  assert.deepEqual(fetched, ["https://x/a.png"]);
  finish("data:a");
  assert.deepEqual(await Promise.all([first, second]), ["data:a", "data:a"]);
  assert.equal(imageLoading("chat", "https://x/a.png"), false);
  assert.equal(loadedImage("chat", "https://x/a.png"), "data:a");
  const failing = loadImage("chat", "https://x/b.png", fetch);
  fail(new Error("refused"));
  await assert.rejects(failing, /refused/);
  assert.equal(imageLoading("chat", "https://x/b.png"), false);
  void loadImage("chat", "https://x/b.png", fetch).catch(() => {});
  assert.deepEqual(fetched, ["https://x/a.png", "https://x/b.png", "https://x/b.png"]);
  fail(new Error("again"));
  forgetImages();
});

test("every reply hears when a fetch settles, including one drawn after it began", async () => {
  forgetImages();
  const heard: [string, string, string | undefined, boolean][] = [];
  let finish!: (data: string) => void;
  let fail!: (error: Error) => void;
  const fetch = () =>
    new Promise<string>((resolve, reject) => {
      finish = resolve;
      fail = reject;
    });
  const clicked = loadImage("chat", "https://x/a.png", fetch);
  // The row scrolls back into view while the fetch runs: a new reply listens.
  const stop = onImageSettled((scope, src, data) => heard.push([scope, src, data, imageLoading(scope, src)]));
  const gone = onImageSettled(() => heard.push(["unmounted", "", undefined, false]));
  gone();
  finish("data:a");
  await clicked;
  // It hears the image, after the fetch is over, so it draws the image and not a disabled button.
  assert.deepEqual(heard, [["chat", "https://x/a.png", "data:a", false]]);
  const failing = loadImage("chat", "https://x/b.png", fetch);
  fail(new Error("refused"));
  await assert.rejects(failing, /refused/);
  // A failure reaches it too, so its button takes clicks again.
  assert.deepEqual(heard[1], ["chat", "https://x/b.png", undefined, false]);
  stop();
  forgetImages();
});

test("a click loads only addresses that are on the host its button names", () => {
  assert.deepEqual(
    loadableSources(
      [
        { host: "github.com", src: "https://github.com/a.png" },
        // A placeholder that says github.com but points elsewhere is never fetched.
        { host: "github.com", src: "https://evil.example/?d=SECRET" },
        { host: "github.com", src: "https://github.com.evil.example/x.png" },
        { host: "github.com", src: "https://github.com/a.png" },
        { host: "github.com", src: "https://github.com/b.png" },
        { host: "images.test", src: "https://images.test/c.png" },
        { host: "github.com" },
        { src: "https://github.com/d.png" },
      ],
      "github.com",
    ),
    ["https://github.com/a.png", "https://github.com/b.png"],
  );
});

test("a reply keeps none of the attributes and classes placeholders use", () => {
  assert.equal(replyAttribute("data-remote-src", "https://evil.example/"), undefined);
  assert.equal(replyAttribute("data-remote-host", "github.com"), undefined);
  assert.equal(replyAttribute("DATA-REMOTE-ALT", "x"), undefined);
  assert.equal(replyAttribute("class", "remote-image"), undefined);
  assert.equal(replyAttribute("class", "note  remote-image-load big REMOTE-IMAGE-alt"), "note big");
  // Anything else stays as written.
  assert.equal(replyAttribute("class", "remoteimage  wide"), "remoteimage  wide");
  assert.equal(replyAttribute("data-note", "kept"), "kept");
  assert.equal(replyAttribute("title", "remote-image"), "remote-image");
});

test("only http and https addresses without credentials are fetched", () => {
  assert.equal(imageAddress("https://images.example/a.png#top").href, "https://images.example/a.png");
  assert.equal(imageAddress("http://images.example/a.png").protocol, "http:");
  for (const value of ["file:///etc/hosts", "javascript:alert(1)", "data:image/png;base64,AA", "ftp://x/a.png", "https://user:pw@x/a.png", "not a url", 42, undefined, "https://x/" + "a".repeat(9000)])
    assert.throws(() => imageAddress(value), String(value).slice(0, 40));
});

test("this computer, its network and other non-internet addresses are private", () => {
  for (const address of [
    "127.0.0.1", "10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.1.1", "169.254.169.254",
    "100.64.0.1", "0.0.0.0", "224.0.0.1", "255.255.255.255", "192.0.0.8", "198.18.0.1",
    "::", "::1", "fe80::1", "fe80::1%en0", "fc00::1", "fd12:3456::1", "ff02::1",
    "::ffff:127.0.0.1", "::ffff:7f00:1", "::ffff:10.0.0.1", "64:ff9b::a9fe:a9fe", "2002:c0a8:101::1",
    "[::1]", "not an address",
  ])
    assert.equal(privateAddress(address), true, address);
  for (const address of ["8.8.8.8", "1.1.1.1", "172.32.0.1", "100.128.0.1", "2606:4700::1111", "::ffff:8.8.8.8", "2a00:1450:4001::200e"])
    assert.equal(privateAddress(address), false, address);
});

test("only image types are taken", () => {
  assert.equal(imageType("image/png; charset=binary"), "image/png");
  assert.equal(imageType("IMAGE/SVG+XML"), "image/svg+xml");
  assert.equal(imageType(["image/webp"]), "image/webp");
  for (const type of ["text/html", "application/octet-stream", "image/x-unknown", "", undefined])
    assert.equal(imageType(type), undefined, String(type));
});

const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
  "base64",
);

async function fixture() {
  const seen: IncomingHttpHeaders[] = [];
  const server: Server = createServer((request, response) => {
    seen.push(request.headers);
    const path = request.url ?? "";
    if (path === "/pixel.png") {
      response.writeHead(200, { "content-type": "image/png", "set-cookie": "track=1" });
      return response.end(PNG);
    }
    if (path === "/page.html") {
      response.writeHead(200, { "content-type": "text/html" });
      return response.end("<p>not an image</p>");
    }
    if (path === "/large.png") {
      response.writeHead(200, { "content-type": "image/png", "content-length": "5000" });
      return response.end(Buffer.alloc(5000));
    }
    if (path === "/streamed.png") {
      // No length up front: the cap applies while it arrives.
      response.writeHead(200, { "content-type": "image/png" });
      response.write(Buffer.alloc(3000));
      return response.end(Buffer.alloc(3000));
    }
    if (path === "/slow.png") return void setTimeout(() => response.end(), 2000);
    if (path === "/moved") {
      response.writeHead(302, { location: "/pixel.png" });
      return response.end();
    }
    if (path === "/inside") {
      response.writeHead(302, { location: `http://127.0.0.1:${port}/pixel.png` });
      return response.end();
    }
    if (path === "/missing.png") {
      response.writeHead(404);
      return response.end();
    }
    response.writeHead(500);
    response.end();
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = (server.address() as AddressInfo).port;
  return { server, port, seen, base: `http://images.test:${port}` };
}

test("an image is fetched as a data URL, with no cookie or referrer, and only from an allowed host", async () => {
  const { server, port, seen, base } = await fixture();
  const hosts = { "images.test": "127.0.0.1" };
  try {
    const data = await fetchRemoteImage(`${base}/pixel.png`, { hosts });
    assert.equal(data, `data:image/png;base64,${PNG.toString("base64")}`);
    assert.equal(seen[0].cookie, undefined);
    assert.equal(seen[0].referer, undefined);
    assert.equal(seen[0]["user-agent"], "Haarness Desktop");
    // Redirects are followed and checked again.
    assert.equal(await fetchRemoteImage(`${base}/moved`, { hosts }), data);
    await assert.rejects(fetchRemoteImage(`${base}/inside`, { hosts }), /on this computer or its network/);
    // Without the pinned name, this computer is refused before any request.
    const before = seen.length;
    await assert.rejects(fetchRemoteImage(`http://127.0.0.1:${port}/pixel.png`), /on this computer or its network/);
    await assert.rejects(fetchRemoteImage(`http://localhost:${port}/pixel.png`), /on this computer or its network/);
    await assert.rejects(fetchRemoteImage(`http://[::1]:${port}/pixel.png`), /on this computer or its network/);
    assert.equal(seen.length, before);
    await assert.rejects(fetchRemoteImage(`${base}/page.html`, { hosts }), /not an image/);
    await assert.rejects(fetchRemoteImage(`${base}/missing.png`, { hosts }), /answered 404/);
    await assert.rejects(fetchRemoteImage(`${base}/large.png`, { hosts, limit: 4096 }), /larger than/);
    await assert.rejects(fetchRemoteImage(`${base}/streamed.png`, { hosts, limit: 4096 }), /larger than/);
    await assert.rejects(fetchRemoteImage(`${base}/slow.png`, { hosts, timeout: 200 }), /took too long/);
    await assert.rejects(fetchRemoteImage("file:///etc/hosts", { hosts }), /Only http and https/);
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
});
