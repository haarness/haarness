import test from "node:test";
import assert from "node:assert/strict";
import { channelsFor, compareVersions, isPrerelease, parseRelease, pickArtifact } from "../src/updates";

test("versions compare as SemVer, and a malformed one never looks newer", () => {
  assert.ok(compareVersions("0.1.6", "0.1.5") > 0);
  assert.ok(compareVersions("0.1.5", "0.1.6") < 0);
  assert.ok(compareVersions("0.2.0", "0.1.99") > 0);
  assert.ok(compareVersions("1.0.0", "0.99.99") > 0);
  assert.equal(compareVersions("0.1.5", "0.1.5"), 0);
  assert.equal(compareVersions("v0.1.5", "0.1.5+build.7"), 0);
  // A release outranks its prereleases; prereleases order by identifiers.
  assert.ok(compareVersions("0.2.0", "0.2.0-preview.3") > 0);
  assert.ok(compareVersions("0.2.0-preview.10", "0.2.0-preview.9") > 0);
  assert.ok(compareVersions("0.2.0-preview.1", "0.2.0-preview") > 0);
  assert.ok(compareVersions("0.2.0-alpha", "0.2.0-1") > 0);
  assert.ok(compareVersions("0.2.0-beta", "0.2.0-alpha") > 0);
  for (const bad of ["", "latest", "1.2", "1.2.3.4", "01.x.0"]) {
    assert.ok(compareVersions(bad, "0.0.1") < 0, bad);
    assert.ok(compareVersions("0.0.1", bad) > 0, bad);
  }
  assert.equal(isPrerelease("0.2.0-preview.1"), true);
  assert.equal(isPrerelease("0.2.0"), false);
  assert.deepEqual(channelsFor("0.1.5"), ["stable"]);
  assert.deepEqual(channelsFor("0.2.0-preview.1"), ["preview", "stable"]);
});

const artifact = (extra: Record<string, unknown> = {}) => ({
  id: "01a1749d-c93a-7799-b748-a5ea95867f25",
  os: "macos",
  url: "https://dl.haarness.ai/releases/01a1749d-c93a-7799-b748-a5ea95867f25/Haarness-0.1.6-mac-arm64.zip",
  arch: "arm64",
  file: "Haarness-0.1.6-mac-arm64.zip",
  kind: "zip",
  size: 212899106,
  digest: { sha256: "e70c3e4740cd863183f833c9131f48ced78392af908ffc72a2fdc139a05f7675" },
  ...extra,
});
const answer = (desktop: unknown) => ({
  channel: "stable",
  manifest: { channel: "stable", version: "0.1.6", products: { cli: {}, desktop } },
});

test("the release service's manifest gives the Desktop release and only complete artifacts", () => {
  const release = parseRelease(
    answer({
      tag: "v0.1.6",
      version: "0.1.6",
      notes_url: "https://haarness.ai/changelog#0.1.6",
      artifacts: [
        artifact(),
        artifact({ kind: "dmg", file: "Haarness-0.1.6-mac-arm64.dmg" }),
        artifact({ digest: {} }),
        artifact({ digest: { sha256: "E70C" } }),
        artifact({ size: 0 }),
        artifact({ size: "12" }),
        artifact({ file: "../Haarness.zip" }),
        artifact({ file: ".hidden.zip" }),
        artifact({ id: "../../x" }),
        null,
        "zip",
      ],
    }),
  );
  assert.equal(release?.version, "0.1.6");
  assert.equal(release?.notesUrl, "https://haarness.ai/changelog#0.1.6");
  assert.deepEqual(
    release?.artifacts.map((item) => item.kind),
    ["zip", "dmg"],
  );
  assert.equal(release?.artifacts[0].sha256, "e70c3e4740cd863183f833c9131f48ced78392af908ffc72a2fdc139a05f7675");
  assert.equal(pickArtifact(release!, { os: "macos", arch: "arm64", kind: "zip" })?.file, "Haarness-0.1.6-mac-arm64.zip");
  assert.equal(pickArtifact(release!, { os: "macos", arch: "x64", kind: "zip" }), undefined);
  assert.equal(pickArtifact(release!, { os: "linux", arch: "arm64", kind: "zip" }), undefined);
  // Notes only from a web page over HTTPS.
  assert.equal(parseRelease(answer({ version: "0.1.6", artifacts: [], notes_url: "javascript:alert(1)" }))?.notesUrl, undefined);
  assert.equal(parseRelease(answer({ version: "0.1.6", artifacts: [], notes_url: null }))?.notesUrl, undefined);
});

test("a channel without Desktop has no release, and an answer that is no manifest throws", () => {
  assert.equal(parseRelease({ manifest: { version: "0.1.6", products: { cli: {} } } }), null);
  assert.throws(() => parseRelease(null));
  assert.throws(() => parseRelease({ error: "not found" }));
  assert.throws(() => parseRelease("<html>"));
  assert.throws(() => parseRelease(answer({ version: "latest", artifacts: [] })));
  // The product's version wins; the manifest's stands in when the product has none.
  assert.equal(parseRelease({ manifest: { version: "0.1.7", products: { desktop: { artifacts: [] } } } })?.version, "0.1.7");
});
