import test from "node:test";
import assert from "node:assert/strict";
import { droppedPath, pathText } from "../src/renderer/file-drop";
import { attachable, attachmentBytes, checkAttachments, relayAttachmentLimit, relayRefusal } from "../src/renderer/media";
import { applyLanguage } from "../src/renderer/i18n";

test("a dropped file that is not media is referred to by its path at the caret", () => {
  const workspace = "/Users/me/project";
  // Inside the project: relative. Outside: as it is. A space: quoted.
  assert.equal(
    pathText(["/Users/me/project/src/a.ts", "/tmp/notes.md", "/Users/me/My Docs/b.md"], workspace, "", ""),
    'src/a.ts /tmp/notes.md "/Users/me/My Docs/b.md"',
  );
  // One space between the path and the words it lands between, no more.
  assert.equal(pathText(["/x/a.md"], workspace, "see", "now"), " /x/a.md ");
  assert.equal(pathText(["/x/a.md"], workspace, "see ", " now"), "/x/a.md");
  assert.equal(pathText(["/Users/me/project/a.md"], workspace + "/", "", ""), "a.md");
  // A folder of the same name prefix is not inside the project.
  assert.equal(pathText(["/Users/me/project-old/a.md"], workspace, "", ""), "/Users/me/project-old/a.md");
});

test("a dropped file's path comes from Electron, else from the drop's file URL of the same name", () => {
  const file = new File(["# spec"], "SPEC v2.md", { type: "text/markdown" });
  assert.equal(droppedPath(file, "", "/real/SPEC v2.md"), "/real/SPEC v2.md");
  const uris = "https://example.com/SPEC v2.md\r\nfile:///Users/me/Desktop/other.md\r\nfile:///Users/me/Desktop/SPEC%20v2.md";
  assert.equal(droppedPath(file, uris), "/Users/me/Desktop/SPEC v2.md");
  // A network share or nothing at all names no path.
  assert.equal(droppedPath(file, "file://server/share/SPEC%20v2.md"), "");
  assert.equal(droppedPath(file, ""), "");
});

test("images, PDFs and other media attach; source code, text and folders do not", async () => {
  const png = Uint8Array.from(atob("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aQx8AAAAASUVORK5CYII="), (c) => c.charCodeAt(0));
  assert.equal(await attachable(new File([png], "shot.png", { type: "image/png" })), true);
  // An image by its bytes, whatever its name lacks.
  assert.equal(await attachable(new File([png], "shot", { type: "" })), true);
  // Named as an image: it goes to the attachment check, which refuses it.
  assert.equal(await attachable(new File(["no"], "fake.png", { type: "image/png" })), true);
  assert.equal(await attachable(new File(["%PDF"], "a.pdf", { type: "application/pdf" })), true);
  assert.equal(await attachable(new File(["x"], "a.ts", { type: "video/mp2t" })), false);
  assert.equal(await attachable(new File(["# x"], "a.md", { type: "text/markdown" })), false);
  assert.equal(await attachable(new File([], "folder", { type: "" })), false);
});

test("files for another computer stop at what the relay carries; a server here takes more", async () => {
  const pdf = (mib: number) => ({
    name: "scan.pdf",
    data: "data:application/pdf;base64," + Buffer.alloc(mib * 1024 * 1024, 1).toString("base64"),
  });
  const twelve = [pdf(12)];
  // This computer's server takes a 12 MiB draft as it is.
  assert.deepEqual(checkAttachments(twelve), twelve);
  // Another computer does not, and says what it takes.
  assert.equal(attachmentBytes(twelve), 12 * 1024 * 1024);
  // Each size is set apart, so a right-to-left sentence keeps it in one piece.
  const apart = (size: string) => "\u2068" + size + "\u2069";
  assert.equal(
    relayRefusal(attachmentBytes(twelve)),
    `The attachments come to ${apart("12 MiB")}, over the ${apart("11 MiB")} limit for media sent to another computer`,
  );
  assert.equal(relayRefusal(relayAttachmentLimit), undefined);
  assert.equal(
    relayRefusal(relayAttachmentLimit + 1),
    `The attachments come to ${apart("11.1 MiB")}, over the ${apart("11 MiB")} limit for media sent to another computer`,
  );
  // In the reader's language, with its own decimal mark.
  const global = globalThis as { document?: unknown };
  const saved = global.document;
  global.document = { documentElement: {} };
  try {
    applyLanguage("de");
    assert.equal(
      relayRefusal(12.35 * 1024 * 1024),
      `Die Anhänge umfassen ${apart("12,4 MiB")} und überschreiten damit das Limit von ${apart("11 MiB")} für Medien an einen anderen Computer`,
    );
  } finally {
    applyLanguage("en");
    global.document = saved;
  }
});
