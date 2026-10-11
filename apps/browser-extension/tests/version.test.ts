import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createManifest, EXTENSION_ID, EXTENSION_KEY, extensionId, workspaceVersion } from "../manifest.config";
import { compareVersions, manifestVersion } from "../src/core/version";

test("workspace versions map onto Chrome's four numbers, previews before releases", () => {
  assert.equal(manifestVersion("0.1.1-preview.1"), "0.1.1.1");
  assert.equal(manifestVersion("0.1.1-preview.12"), "0.1.1.12");
  assert.equal(manifestVersion("0.1.1"), "0.1.1.1000");
  assert.equal(manifestVersion("2.10.0-beta.3"), "2.10.0.3");
  assert.throws(() => manifestVersion("0.1"), /Cannot map/);
  assert.throws(() => manifestVersion("0.1.1-preview.1000"), /below 1000/);
  assert.ok(compareVersions("0.1.1", "0.1.1-preview.9") > 0);
  assert.ok(compareVersions("0.1.2-preview.1", "0.1.1") > 0);
  assert.equal(compareVersions("0.1.1-preview.1", "0.1.1-preview.1"), 0);
  assert.ok(compareVersions("0.1.1-preview.1", "0.1.1-preview.2") < 0);
});

test("the manifest key pins the extension ID native messaging allows", () => {
  assert.equal(extensionId(EXTENSION_KEY), EXTENSION_ID);
  assert.equal(EXTENSION_ID, "accdjhmcgljbadpeanajllmcembhkomn");
  // The signing key folder sits beside the checkout on a maintainer's machine.
  const local = new URL("../../../../.chrome-extension-key/pub.b64", import.meta.url);
  try {
    assert.equal(readFileSync(local, "utf8").trim(), EXTENSION_KEY);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }
});

test("the manifest asks for exactly what the extension uses", () => {
  const version = workspaceVersion(new URL("../../../Cargo.toml", import.meta.url).pathname);
  const manifest = createManifest(version);
  assert.equal(manifest.version_name, version);
  assert.equal(manifest.version, manifestVersion(version));
  assert.deepEqual(manifest.permissions, ["debugger", "tabs", "tabGroups", "nativeMessaging", "storage", "alarms", "downloads"]);
  assert.equal("host_permissions" in manifest, false);
  assert.equal("web_accessible_resources" in manifest, false);
  assert.equal("externally_connectable" in manifest, false);
  assert.equal("content_scripts" in manifest, false);
  assert.equal(manifest.incognito, "not_allowed");
  assert.equal(manifest.minimum_chrome_version, "125");
  assert.equal(manifest.name, "Haarness");
  assert.deepEqual(manifest.commands["stop-all"].suggested_key, { default: "Alt+Shift+S" });
});
