import test from "node:test";
import assert from "node:assert/strict";
import { appendFile, mkdtemp, rename, rm, symlink, truncate, writeFile } from "node:fs/promises";
import { attachments } from "@haarness/sdk";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { listScreenshots, readListedScreenshots } from "../src/main/screenshot-files";
import {
  SCREENSHOT_LIMIT,
  createScreenshotIntents,
  inputKind,
  createScreenshotListings,
  displayFolder,
  imageName,
  newestImages,
  parseDefaultsLocation,
  screenshotFolder,
} from "../src/screenshots";
import { relativeTime, toggleSelection } from "../src/renderer/screenshot-picker";

test("macOS's Save to setting names the folder, quoted or with ~", () => {
  assert.equal(parseDefaultsLocation('"/Users/me/My Shots"\n', "/Users/me"), "/Users/me/My Shots");
  assert.equal(parseDefaultsLocation("/Users/me/Shots\n"), "/Users/me/Shots");
  assert.equal(parseDefaultsLocation("~/Pictures/Shots\n", "/Users/me"), "/Users/me/Pictures/Shots");
  assert.equal(parseDefaultsLocation("~\n", "/Users/me"), "/Users/me");
  assert.equal(parseDefaultsLocation("~/x", undefined), undefined);
  // `defaults` prints nothing when the setting was never changed; a relative path is not a folder.
  for (const output of ["", "\n", '""', "Pictures"]) assert.equal(parseDefaultsLocation(output, "/Users/me"), undefined);
});

test("the picker opens the system's screenshots folder, and on Linux the one chosen last", () => {
  assert.equal(screenshotFolder({ platform: "darwin", home: "/Users/me" }), "/Users/me/Desktop");
  assert.equal(
    screenshotFolder({ platform: "darwin", home: "/Users/me", location: "/Users/me/Shots" }),
    "/Users/me/Shots",
  );
  assert.equal(
    screenshotFolder({ platform: "win32", home: "C:\\Users\\me" }),
    "C:\\Users\\me\\Pictures\\Screenshots",
  );
  assert.equal(screenshotFolder({ platform: "linux", home: "/home/me" }), undefined);
  assert.equal(screenshotFolder({ platform: "linux", home: "/home/me", remembered: "/home/me/Shots" }), "/home/me/Shots");
  assert.equal(screenshotFolder({ platform: "linux", home: "/home/me", remembered: 7 }), undefined);
  // macOS and Windows ignore a folder remembered on Linux.
  assert.equal(screenshotFolder({ platform: "darwin", home: "/Users/me", remembered: "/tmp/x" }), "/Users/me/Desktop");
  assert.equal(displayFolder("/Users/me/Desktop", "/Users/me"), "~/Desktop");
  assert.equal(displayFolder("C:\\Users\\me\\Pictures\\Screenshots", "C:\\Users\\me"), "~\\Pictures\\Screenshots");
  assert.equal(displayFolder("/Volumes/Shots", "/Users/me"), "/Volumes/Shots");
  assert.equal(displayFolder("/Users/meg/Desktop", "/Users/me"), "/Users/meg/Desktop");
});

test("only images the attachment reader takes are offered, newest first and at most a listing's worth", () => {
  for (const name of ["Screenshot 2026-10-18 at 10.00.00.png", "a.JPG", "b.jpeg", "c.webp", "d.heic", "e.tiff", "f.gif"])
    assert.ok(imageName(name), name);
  for (const name of [".hidden.png", "notes.txt", "clip.mov", "archive.png.zip", "png", "report.pdf"])
    assert.ok(!imageName(name), name);
  const entries = [
    { name: "old.png", modified: 1, file: true },
    { name: "new.png", modified: 3, file: true },
    { name: "middle.jpg", modified: 2, file: true },
    { name: "folder.png", modified: 9, file: false },
    { name: "notes.txt", modified: 8, file: true },
    { name: ".DS_Store", modified: 7, file: true },
  ];
  assert.deepEqual(newestImages(entries).map((entry) => entry.name), ["new.png", "middle.jpg", "old.png"]);
  assert.deepEqual(newestImages(entries, 2).map((entry) => entry.name), ["new.png", "middle.jpg"]);
  const many = Array.from({ length: 80 }, (_, index) => ({ name: `s${index}.png`, modified: index, file: true }));
  const listed = newestImages(many);
  assert.equal(listed.length, SCREENSHOT_LIMIT);
  assert.equal(listed[0].name, "s79.png");
});

test("a pick names places in the window's last listing, and nothing else", () => {
  const listings = createScreenshotListings();
  const first = listings.remember(1, ["/shots/a.png", "/shots/b.png", "/shots/c.png"]);
  assert.deepEqual(listings.resolve(1, first, [2, 0]), ["/shots/c.png", "/shots/a.png"]);
  // Another window's listing, or one this window has since replaced, is refused.
  assert.throws(() => listings.resolve(2, first, [0]), /changed/);
  const second = listings.remember(1, ["/other/x.png"]);
  assert.throws(() => listings.resolve(1, first, [0]), /changed/);
  assert.deepEqual(listings.resolve(1, second, [0]), ["/other/x.png"]);
  for (const indexes of [[], [1], [-1], [0, 0], [0.5], ["0"], "0", null, Array.from({ length: 21 }, (_, index) => index)])
    assert.throws(() => listings.resolve(1, second, indexes), /up to 20/, JSON.stringify(indexes));
  assert.throws(() => listings.resolve(1, "/other/x.png", [0]), /changed/);
  listings.forget(1);
  assert.throws(() => listings.resolve(1, second, [0]), /changed/);
});

test("a screenshot swapped for a link or another file after it was listed is refused, not read", async () => {
  const folder = await mkdtemp(join(tmpdir(), "haarness-screenshots-"));
  try {
    for (const [name, text] of [["a.png", "first"], ["b.png", "second"], ["c.png", "third"], ["d.png", "fourth"], ["private.png", "another file"]])
      await writeFile(join(folder, name), text);
    await symlink(join(folder, "private.png"), join(folder, "link.png"));
    // Over what one message may attach (sparse, so it takes no room).
    await writeFile(join(folder, "big.png"), "");
    await truncate(join(folder, "big.png"), 20 * 1024 * 1024 + 1);
    const listed = await listScreenshots(folder);
    // A link is never listed.
    assert.deepEqual(
      listed.map((entry) => entry.name).sort(),
      ["a.png", "b.png", "big.png", "c.png", "d.png", "private.png"],
    );
    const pick = (name: string) => listed.find((entry) => entry.name === name)!;
    const text = (values: string[]) => values.map((value) => Buffer.from(value.split(",")[1], "base64").toString());
    assert.deepEqual(text(await readListedScreenshots([pick("b.png"), pick("a.png")])), ["second", "first"]);
    // The same attachments the SDK's reader makes from the paths.
    assert.deepEqual(await readListedScreenshots([pick("b.png"), pick("a.png")]), await attachments([pick("b.png").path, pick("a.png").path]));
    await assert.rejects(readListedScreenshots([pick("a.png"), pick("big.png")]), /20 MiB/);
    // Made a link to another file since the listing.
    await rm(join(folder, "a.png"));
    await symlink(join(folder, "private.png"), join(folder, "a.png"));
    await assert.rejects(readListedScreenshots([pick("b.png"), pick("a.png")]), /changed/);
    // Rewritten since the listing.
    await writeFile(join(folder, "b.png"), "rewritten");
    await assert.rejects(readListedScreenshots([pick("b.png")]), /changed/);
    // Swapped for a link after it was checked: what is read is still the listed file, never the link's target.
    const swapped = await readListedScreenshots([pick("c.png")], {
      checked: async (path) => {
        await rename(path, path + ".moved");
        await symlink(join(folder, "private.png"), path);
      },
    });
    assert.deepEqual(text(swapped), ["third"]);
    // Written to while it was read.
    await assert.rejects(
      readListedScreenshots([pick("d.png")], { checked: (path) => appendFile(path, " and more") }),
      /changed/,
    );
  } finally {
    await rm(folder, { recursive: true, force: true });
  }
});

test("the folder is listed for a choice in the Attach menu, not after any press in the window", () => {
  const kinds: [object, string | undefined][] = [
    [{ type: "mouseDown" }, "press"],
    [{ type: "keyDown", key: "Enter" }, "press"],
    [{ type: "rawKeyDown", key: " " }, "press"],
    [{ type: "touchEnd" }, "press"],
    [{ type: "gestureTap" }, "press"],
    [{ type: "keyDown", key: "ArrowDown" }, "arrow"],
    [{ type: "keyDown", key: "a" }, "typing"],
    [{ type: "keyDown", key: "é" }, "typing"],
    [{ type: "mouseMove" }, undefined],
    [{ type: "mouseUp" }, undefined],
    [{ type: "keyDown", key: "Escape" }, undefined],
    [{ type: "keyDown", key: "ArrowUp" }, undefined],
    [{ type: "keyUp", key: "Enter" }, undefined],
    [{ type: "char", key: "a" }, undefined],
  ];
  for (const [input, kind] of kinds) assert.equal(inputKind(input as { type: string }), kind, JSON.stringify(input));
  let now = 1000;
  const intents = createScreenshotIntents(() => now);
  // Nothing pressed: no menu token, and a listing without one is not a choice.
  assert.equal(intents.menu(1), null);
  assert.equal(intents.chosen(1, undefined), false);
  // A click opens the menu, a second chooses Attach screenshots: listed once.
  intents.input(1, "press");
  const token = intents.menu(1)!;
  assert.ok(token);
  now += 3000;
  intents.input(1, "press");
  assert.equal(intents.chosen(1, token), true);
  assert.equal(intents.chosen(1, token), false);
  // From the keyboard: Enter or ArrowDown opens it, ArrowDown moves, Enter chooses.
  intents.input(1, "arrow");
  const keyboard = intents.menu(1)!;
  intents.input(1, "arrow");
  intents.input(1, "press");
  assert.equal(intents.chosen(1, keyboard), true);
  // The press that sends a message, and a page asking for a menu right after
  // it: no later press, so nothing is chosen.
  intents.input(1, "press");
  assert.equal(intents.chosen(1, intents.menu(1)), false);
  // Something typed between the menu and the press.
  intents.input(1, "press");
  const typed = intents.menu(1);
  intents.input(1, "typing");
  intents.input(1, "press");
  assert.equal(intents.chosen(1, typed), false);
  // A menu not opened by a press, or opened too long after one.
  intents.input(1, "typing");
  assert.equal(intents.menu(1), null);
  intents.input(1, "press");
  now += 2001;
  assert.equal(intents.menu(1), null);
  // A choice too long after its press, or after the menu.
  intents.input(1, "press");
  const slow = intents.menu(1);
  intents.input(1, "press");
  now += 2001;
  assert.equal(intents.chosen(1, slow), false);
  intents.input(1, "press");
  const stale = intents.menu(1);
  now += 15_001;
  intents.input(1, "press");
  assert.equal(intents.chosen(1, stale), false);
  // Another window's menu or token, a made-up token, or a closed window.
  intents.input(1, "press");
  const first = intents.menu(1);
  intents.input(2, "press");
  intents.input(2, "press");
  assert.equal(intents.chosen(2, first), false);
  intents.input(1, "press");
  assert.equal(intents.chosen(1, "made-up"), false);
  intents.input(1, "press");
  const closed = intents.menu(1);
  intents.forget(1);
  intents.input(1, "press");
  assert.equal(intents.chosen(1, closed), false);
});

test("picked screenshots are numbered in the order they were clicked", () => {
  let picked: number[] = [];
  for (const index of [4, 1, 7]) picked = toggleSelection(picked, index);
  assert.deepEqual(picked, [4, 1, 7]);
  // Clicking one again takes it out and closes the gap.
  picked = toggleSelection(picked, 1);
  assert.deepEqual(picked, [4, 7]);
  assert.deepEqual(toggleSelection([0, 1], 2, 2), [0, 1]);
  assert.deepEqual(toggleSelection([0, 1], 1, 2), [0]);
});

test("a screenshot says how long ago it was taken, in the reader's language", () => {
  const now = Date.UTC(2026, 9, 18, 12);
  assert.equal(relativeTime(now - 10_000, now, "en"), "now");
  assert.equal(relativeTime(now - 5 * 60_000, now, "en"), "5 minutes ago");
  assert.equal(relativeTime(now - 3 * 3_600_000, now, "en"), "3 hours ago");
  assert.equal(relativeTime(now - 24 * 3_600_000, now, "en"), "yesterday");
  assert.equal(relativeTime(now - 40 * 86_400_000, now, "en"), "last month");
  assert.equal(relativeTime(now - 5 * 60_000, now, "de"), "vor 5 Minuten");
});
