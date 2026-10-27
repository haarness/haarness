import test from "node:test";
import assert from "node:assert/strict";
import { restoreArgs, restoredMessage, restorePlan, restoreSummary } from "../src/renderer/profile-restore";

// The name is wrapped in invisible bidi isolates; compare the text a reader sees.
const visible = (text: string) => text.replace(/[\u2068\u2069]/g, "");

// `haarness profile restore <file> --dry-run --json`, as the CLI prints it.
const preview = {
  name: "work",
  dryRun: true,
  targets: ["/home/a/.config/haarness/config.toml", "/w/.haarness/config.toml", "/w/.haarness/agents/review.md"],
  overwrite: ["/w/.haarness/config.toml"],
};

test("a preview names every file and puts the ones it replaces first", () => {
  const summary = restoreSummary(restorePlan(preview));
  assert.equal(visible(summary.description), "Restoring “work” writes 3 files. 1 file already exists and will be replaced.");
  assert.deepEqual(summary.list, [
    { text: "/w/.haarness/config.toml", tag: "Will be replaced" },
    { text: "/home/a/.config/haarness/config.toml" },
    { text: "/w/.haarness/agents/review.md" },
  ]);
  assert.equal(summary.replaces, true);
  assert.equal(
    visible(restoreSummary(restorePlan({ ...preview, targets: ["/w/a.md"], overwrite: ["/w/a.md"] })).description),
    "Restoring “work” writes 1 file. It already exists and will be replaced.",
  );
  const fresh = restoreSummary(restorePlan({ ...preview, targets: ["/w/a.md"], overwrite: [] }));
  assert.equal(visible(fresh.description), "Restoring “work” writes 1 file. No existing file is replaced.");
  assert.equal(fresh.replaces, false);
  assert.match(
    restoreSummary(restorePlan({ ...preview, overwrite: preview.targets })).description,
    /3 files already exist and will be replaced\.$/,
  );
});

test("the name is isolated, so an Arabic sentence keeps a Latin name and its count in order", () => {
  const plan = restorePlan(preview);
  assert.match(restoreSummary(plan).description, /“\u2068work\u2069”/);
  assert.equal(restoredMessage(plan), "Profile “\u2068work\u2069” restored.");
});

test("the confirmed restore replaces files only when the preview listed some", () => {
  assert.deepEqual(restoreArgs(["restore", "/p/work.profile.zip"], restorePlan(preview)), [
    "profile", "restore", "/p/work.profile.zip", "--force", "--json",
  ]);
  assert.deepEqual(
    restoreArgs(["pull", "id", "--url", "https://h.example"], restorePlan({ ...preview, overwrite: [] })),
    ["profile", "pull", "id", "--url", "https://h.example", "--json"],
  );
});

test("anything but a dry-run answer is refused rather than restored blindly", () => {
  for (const value of [null, "text", { ...preview, dryRun: false }, { name: "x", dryRun: true }])
    assert.throws(() => restorePlan(value), /could not preview/);
});
