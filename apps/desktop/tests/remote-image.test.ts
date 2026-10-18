import test from "node:test";
import assert from "node:assert/strict";
import { createServer, type IncomingHttpHeaders, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { fetchRemoteImage, imageAddress, imageType, privateAddress } from "../src/main/remote-image";
import { forgetImages, loadedImage, rememberImage, remoteImageHost } from "../src/renderer/remote-images";

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
