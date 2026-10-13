import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createAttachmentDraftStorage } from "../src/main/attachment-drafts";
import { createAttachmentDrafts } from "../src/renderer/attachment-drafts";
const file = (name: string, bytes = 3) => ({ name, data: "data:image/png;base64," + Buffer.alloc(bytes, 1).toString("base64") });

test("attachment storage preserves large media across reload and serializes deletion/readback", async () => {
  const path = await mkdtemp(join(tmpdir(), "haarness-draft-test-"));
  try {
    const storage = createAttachmentDraftStorage(path);
    const large = [file("large.png", 11 * 1024 * 1024)];
    const writing = storage("new:project-a", large);
    const reading = storage("new:project-a");
    assert.deepEqual(await reading, large);
    await writing;
    assert.deepEqual(await createAttachmentDraftStorage(path)("new:project-a"), large);
    assert.deepEqual(await storage("new:project-b"), []);
    const clear = storage("new:project-a", []);
    assert.deepEqual(await storage("new:project-a"), []);
    await clear;
    assert.deepEqual(await readdir(path), []);
    await assert.rejects(storage("x", [file("too-big", 20 * 1024 * 1024 + 3)]), /20 MiB/);
    await assert.rejects(storage("x", [{ name: "bad", data: "javascript:alert(1)" }]), /Invalid attachment/);
    assert.deepEqual(await readdir(path), []);
  } finally { await rm(path, { recursive: true, force: true }); }
});

test("late hydration merges drops and preserves session isolation and sent-file cleanup", async () => {
  let resolve!: (value: ReturnType<typeof file>[]) => void;
  const loaded = new Promise<ReturnType<typeof file>[]>((done) => resolve = done);
  const disk = new Map<string, ReturnType<typeof file>[]>();
  const drafts = createAttachmentDrafts(async (key, value) => {
    if (value) { disk.set(key, value); return value; }
    return key === "a" ? loaded : [];
  }, () => {});
  const first = file("first.png"), second = file("second.png"), later = file("later.png");
  const loading = drafts.load("a");
  const adding = drafts.update("a", (items) => [...items, second]);
  await drafts.update("b", [file("b.png")]);
  resolve([first]);
  await Promise.all([loading, adding]);
  assert.deepEqual(drafts.get("a"), [first, second]);
  const sent = drafts.get("a");
  await drafts.update("a", (items) => [...items, later]);
  await drafts.update("a", (items) => items.filter((item) => !sent.includes(item)));
  assert.deepEqual(drafts.get("a"), [later]);
  assert.deepEqual(disk.get("a"), [later]);
  assert.deepEqual(drafts.get("b"), [file("b.png")]);
});

test("a failed disk write is reported, keeps the draft in memory, and permits retry", async () => {
  let failing = true;
  const drafts = createAttachmentDrafts(async (_, value) => {
    if (value && failing) throw Error("disk full");
    return value ?? [];
  }, () => {});
  const value = [file("retry.png")];
  await assert.rejects(drafts.update("a", value), /disk full/);
  assert.deepEqual(drafts.get("a"), value);
  failing = false;
  await drafts.update("a", (items) => items);
  assert.deepEqual(drafts.get("a"), value);
});

test("an attached image is typed by its bytes, not its name", async () => {
  const { imageType } = await import("../src/attachments");
  const bytes = (...values: (number | string)[]) =>
    Uint8Array.from(values.flatMap((value) => typeof value === "string" ? [...value].map((char) => char.charCodeAt(0)) : [value]));
  assert.equal(imageType(bytes(0x89, "PNG\r\n\x1a\n", 0, 0)), "image/png");
  assert.equal(imageType(bytes(0xff, 0xd8, 0xff, 0xe0)), "image/jpeg");
  assert.equal(imageType(bytes("GIF89a")), "image/gif");
  assert.equal(imageType(bytes("RIFF", 0, 0, 0, 0, "WEBPVP8 ")), "image/webp");
  assert.equal(imageType(bytes(0, 0, 0, 0x18, "ftypheic", 0, 0, 0, 0)), "image/heic");
  assert.equal(imageType(bytes("II*\0")), "image/tiff");
  assert.equal(imageType(bytes("just some text")), undefined);
});
