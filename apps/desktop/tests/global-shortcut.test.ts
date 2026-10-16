import test from "node:test";
import assert from "node:assert/strict";
import {
  defaultGlobalShortcut,
  globalAccelerator,
  globalShortcutError,
  reservedGlobalShortcut,
} from "../src/global-shortcut";
import { defaults } from "../src/shared";

test("the global shortcut is off until turned on, with Option+Space ready", () => {
  assert.deepEqual(defaults.globalShortcut, { enabled: false, keys: "Alt+Space" });
  assert.equal(defaultGlobalShortcut.enabled, false);
});

test("a global shortcut needs Command, Control or Option and one other key", () => {
  assert.equal(globalAccelerator("Alt+Space"), "Alt+Space");
  assert.equal(globalAccelerator("Mod+Shift+h"), "CommandOrControl+Shift+H");
  assert.equal(globalAccelerator("Ctrl+Alt+O"), "Control+Alt+O");
  assert.equal(globalAccelerator("Alt+F5"), "Alt+F5");
  assert.equal(globalAccelerator("Mod+."), "CommandOrControl+.");
  // Shift alone would take a capital letter from every app.
  assert.equal(globalAccelerator("Shift+A"), undefined);
  assert.equal(globalAccelerator("Space"), undefined);
  assert.equal(globalAccelerator("Alt+Alt+Space"), undefined);
  assert.equal(globalAccelerator("Alt+Tab+Space"), undefined);
  assert.equal(globalAccelerator("Hyper+Space"), undefined);
  assert.equal(globalAccelerator("Alt+Enter"), undefined);
});

test("combinations the system keeps are refused, in any modifier order", () => {
  assert.equal(reservedGlobalShortcut("Mod+Space", "darwin"), true);
  assert.equal(reservedGlobalShortcut("Shift+Mod+4", "darwin"), true);
  assert.equal(reservedGlobalShortcut("Alt+Space", "darwin"), false);
  assert.equal(reservedGlobalShortcut("Alt+Tab", "win32"), true);
  assert.equal(reservedGlobalShortcut("Mod+Space", "win32"), false);
  assert.equal(globalShortcutError("Alt+Space", "darwin"), undefined);
  assert.equal(globalShortcutError("Mod+Space", "darwin"), "⌘ Space belongs to the system. Choose another shortcut.");
  assert.equal(globalShortcutError("Shift+A", "linux"), "Use a modifier key and one more key, such as Alt+Space.");
  assert.equal(
    globalShortcutError("Shift+A", "darwin", () => "Utilisez {keys}."),
    "Utilisez ⌥ Space.",
  );
});
