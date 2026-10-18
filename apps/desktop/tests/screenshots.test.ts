import test from "node:test";
import assert from "node:assert/strict";
import {
  SCREENSHOT_LIMIT,
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
