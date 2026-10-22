import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createSideChatStorage } from "../src/main/side-chat-storage";
import { MAX_CHATS, MAX_STORED } from "../src/renderer/side-chat";

const chat = (question: string) => ({ threads: [{ id: "t", turns: [{ question, answer: "a", state: "done" }], draft: "", time: 0 }] });

test("side chats are kept per chat in files of their own, for the chats used most recently", async () => {
  const directory = await mkdtemp(join(tmpdir(), "haarness-side-chats-"));
  try {
    const storage = createSideChatStorage(directory);
    assert.equal(await storage.get("p:one"), null);
    await storage.set("p:one", chat("first"));
    assert.deepEqual(await storage.get("p:one"), chat("first"));
    // Every save is a use: the chat saved longest ago is the one forgotten.
    for (let at = 0; at < MAX_CHATS; at++) await storage.set(`p:${at}`, chat(String(at)));
    assert.equal(await storage.get("p:one"), null);
    assert.deepEqual(await storage.get("p:0"), chat("0"));
    await storage.set("p:0", chat("again"));
    await storage.set("p:new", chat("new"));
    assert.deepEqual(await storage.get("p:0"), chat("again"));
    assert.equal(await storage.get("p:1"), null);
    // Null forgets one chat; a removed project's chats all go.
    await storage.set("p:0", null);
    assert.equal(await storage.get("p:0"), null);
    await storage.set("q:kept", chat("kept"));
    await storage.drop("p:");
    assert.equal(await storage.get("p:new"), null);
    assert.deepEqual(await storage.get("q:kept"), chat("kept"));
    assert.deepEqual((await readdir(directory)).filter((name) => name !== "index.json").length, 1);
    // Too large, or a bad key, is refused and changes nothing.
    await assert.rejects(storage.set("q:kept", { threads: [{ text: "x".repeat(MAX_STORED * 2) }] }));
    await assert.rejects(storage.get(""));
    assert.deepEqual(await storage.get("q:kept"), chat("kept"));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
