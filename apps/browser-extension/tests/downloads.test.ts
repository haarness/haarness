import assert from "node:assert/strict";
import test from "node:test";
import { fileID } from "../src/core/browser";
import { createAttribution } from "../src/core/downloads";
import { createDownloads, type Finished } from "../src/platform/chrome/downloads";
import { createFileStore, underRoot } from "../src/platform/chrome/files";
import { installChrome } from "./fakes/chrome";

const THREAD = "0192a5f0-7c1e-7d3a-9b1e-4f2a9c3d5e61";

test("a download an agent tab began becomes that tab's file and is announced when complete", async () => {
  const fake = installChrome();
  const items = new Map<number, Partial<chrome.downloads.DownloadItem>>();
  (fake.chrome.downloads as { search: unknown }).search = async ({ id }: { id: number }) => [items.get(id)];
  const attribution = createAttribution();
  const store = createFileStore(() => crypto.randomUUID());
  const downloads = createDownloads({ attribution, store: () => store, existing: () => store, uuid: () => crypto.randomUUID() });
  const finished: Finished[] = [];

  // The user's own download is never the agent's.
  downloads.created({ id: 1, url: "https://a.example/mine.pdf", filename: "", startTime: new Date().toISOString() } as chrome.downloads.DownloadItem);
  assert.equal(store.list().length, 0);

  attribution.began({
    url: "https://a.example/report.pdf",
    tabId: 7,
    command: "c1",
    server: "s1",
    thread: THREAD,
    name: "report.pdf",
    resources: ["https://app.example", "https://a.example"],
  });
  downloads.created({
    id: 2,
    url: "https://a.example/report.pdf",
    finalUrl: "https://cdn.example/report.pdf",
    filename: "/Users/me/Downloads/report.pdf",
    startTime: new Date().toISOString(),
  } as chrome.downloads.DownloadItem);
  assert.deepEqual(store.list().map((file) => [file.name, file.state]), [["report.pdf", "pending"]]);
  assert.throws(() => store.get(store.list()[0].id), /still being downloaded/);

  items.set(2, { id: 2, filename: "/Users/me/Downloads/report.pdf", fileSize: 1234, mime: "application/pdf" });
  await downloads.changed({ id: 2, state: { current: "complete", previous: "in_progress" } } as chrome.downloads.DownloadDelta, (done) => finished.push(done));
  assert.deepEqual(finished, [{ server: "s1", thread: THREAD, command: "c1", path: "/Users/me/Downloads/report.pdf", url: "https://a.example/report.pdf" }]);
  const [file] = store.list();
  assert.equal(file.state, "completed");
  // Getting it is gated on the page it came from and every site its URL passed through.
  assert.deepEqual(store.get(file.id).resources, ["https://a.example", "https://app.example", "https://cdn.example"]);
  assert.deepEqual(await store.transfer(file.id, store.get(file.id).resources), {
    id: file.id,
    name: "report.pdf",
    mime: "application/pdf",
    bytes: 1234,
    path: "/Users/me/Downloads/report.pdf",
  });
  await assert.rejects(store.transfer(file.id, ["https://a.example"]), /source changed/);

  // Captures travel inline, as bytes the host outcome encodes.
  const capture = await store.save("shot.png", "image/png", new Uint8Array([1, 2, 3]), ["https://a.example"]);
  assert.deepEqual(await store.transfer(capture), { id: capture, name: "shot.png", mime: "image/png", data: new Uint8Array([1, 2, 3]) });
  assert.throws(() => store.get(fileID(crypto.randomUUID())), /not retained/);
});

test("uploads come only from inside the bridge's staging folder", () => {
  assert.equal(underRoot("/tmp/up/1234/a.png", "/tmp/up"), true);
  assert.equal(underRoot("/tmp/up/../secrets", "/tmp/up"), false);
  assert.equal(underRoot("/tmp/upload-elsewhere/a", "/tmp/up"), false);
  assert.equal(underRoot("/tmp/up", "/tmp/up"), false);
  assert.equal(underRoot("C:\\Users\\me\\up\\x\\a.png", "C:\\Users\\me\\up"), true);
  assert.equal(underRoot("/tmp/up/a", null), false);
});
