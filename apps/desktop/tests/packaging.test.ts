import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const root = new URL("..", import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("package.json", root), "utf8"));
const entitlements = readFileSync(
  new URL(manifest.build.mac.entitlements, root),
  "utf8",
);

// Commands the agent or the terminal runs are children of Haarness, so macOS
// asks on Haarness's behalf. Without a usage string (and, under the hardened
// runtime, the matching entitlement) the request is refused without a prompt,
// and the app never appears in Privacy & Security.
test("commands run from the app can ask for calendars, reminders, camera and app automation", () => {
  const info = manifest.build.mac.extendInfo as Record<string, string>;
  for (const key of [
    "NSCalendarsUsageDescription",
    "NSCalendarsFullAccessUsageDescription",
    "NSRemindersUsageDescription",
    "NSRemindersFullAccessUsageDescription",
    "NSCameraUsageDescription",
    "NSAppleEventsUsageDescription",
    "NSMicrophoneUsageDescription",
    "NSSpeechRecognitionUsageDescription",
  ])
    assert.match(info[key] ?? "", /^Haarness .+\.$/, key);
  for (const key of [
    "com.apple.security.personal-information.calendars",
    "com.apple.security.device.camera",
    "com.apple.security.automation.apple-events",
    "com.apple.security.device.audio-input",
  ])
    assert.match(entitlements, new RegExp(`<key>${key.replaceAll(".", "\\.")}</key><true/>`), key);
  assert.equal(manifest.build.mac.entitlementsInherit, manifest.build.mac.entitlements);
  assert.equal(manifest.build.mac.hardenedRuntime, true);
});
