import { test } from "node:test";
import assert from "node:assert/strict";
import {
  chatDay,
  chatFolderPath,
  chatSlug,
  folderName,
  newChatFolder,
} from "../src/renderer/chat-folder";

test("a chat's folder is named by up to six words of its first message, folded to lowercase ASCII", () => {
  assert.equal(chatSlug("Plan a trip to Lisbon"), "plan-a-trip-to-lisbon");
  assert.equal(
    chatSlug("Summarise these meeting notes for me please, quickly"),
    "summarise-these-meeting-notes-for-me",
  );
  assert.equal(chatSlug("Crème brûlée für Søren — ß & Æ"), "creme-brulee-fur-soren-ss-ae");
  assert.equal(chatSlug("  /review   the  diff!! "), "review-the-diff");
  assert.equal(chatSlug("Ünïcödé İstanbul"), "unicode-istanbul");
});

test("a chat's folder name stays within 40 characters and never a device name", () => {
  const long = chatSlug("internationalization localization accessibility considerations");
  assert.ok(long.length <= 40, long);
  assert.equal(long, "internationalization-localization");
  assert.equal(chatSlug("a".repeat(60)), "a".repeat(40));
  assert.equal(chatSlug("CON"), "");
  assert.equal(chatSlug("lpt1"), "");
  assert.equal(chatSlug("con artist"), "con-artist");
});

test("a message with no Latin letters, or none at all, gives the chat a folder of its own anyway", () => {
  assert.equal(chatSlug("مرحبا بالعالم"), "");
  assert.equal(chatSlug("你好"), "");
  assert.equal(chatSlug(""), "");
  const date = new Date(2026, 9, 4, 23, 30);
  assert.equal(newChatFolder("你好", date, () => "a1b2"), "2026-10-04/chat-a1b2");
  assert.equal(newChatFolder("", date, () => "ffff"), "2026-10-04/chat-ffff");
  assert.match(newChatFolder("", date), /^2026-10-04\/chat-[0-9a-f]{4}$/);
  assert.equal(newChatFolder("Hello there", date), "2026-10-04/hello-there");
});

test("a chat's folder is filed under the local date it started", () => {
  assert.equal(chatDay(new Date(2026, 0, 2, 0, 1)), "2026-01-02");
  assert.equal(chatDay(new Date(2026, 11, 31, 23, 59)), "2026-12-31");
});

test("the folder a chat reports is shown from the chat home, and only when it is inside it", () => {
  const home = "/Users/me/Library/Application Support/Haarness/Chat folders";
  assert.equal(chatFolderPath(home, home + "/2026-10-24/hello-there"), "2026-10-24/hello-there");
  assert.equal(chatFolderPath(home + "/", home + "/2026-10-24/hello/"), "2026-10-24/hello");
  assert.equal(chatFolderPath(home, home), undefined);
  assert.equal(chatFolderPath(home, home + " 2/2026-10-24/x"), undefined);
  assert.equal(chatFolderPath(home, undefined), undefined);
  assert.equal(
    chatFolderPath("C:\\Users\\me\\Chat folders", "C:\\Users\\me\\Chat folders\\2026-10-24\\notes"),
    "2026-10-24/notes",
  );
  assert.equal(folderName(home + "/2026-10-24/hello-there"), "hello-there");
  assert.equal(folderName("C:\\a\\b\\notes\\"), "notes");
});
