import { test } from "node:test";
import assert from "node:assert/strict";
import { bounded, external, localArgs, request } from "../src/main/validation";

test("external navigation cannot invoke local executables, files or embedded credentials", () => {
  for (const url of [
    "file:///etc/passwd",
    "javascript:alert(1)",
    "data:text/html,test",
    "https://user:secret@example.test",
  ])
    assert.throws(() => external(url));
  assert.equal(
    external("https://example.test/docs"),
    "https://example.test/docs",
  );
  assert.equal(
    external("mailto:hello@example.test"),
    "mailto:hello@example.test",
  );
});
test("local management cannot escape its workspace, trust policy or credential channel", () => {
  for (const option of [
    "--workspace=/tmp/elsewhere",
    "--data-dir=/tmp/elsewhere",
    "--trust-project",
    "--disable-project-config",
    "--yolo",
    "--key=secret",
    "--token=secret",
    "--show-secrets",
    "-w",
    "-s",
  ])
    assert.throws(() => localArgs(["extension", "list", option]));
  assert.throws(() => localArgs(["run", "a task"]));
  assert.throws(() => localArgs(["auth", "set", "provider", "secret"]));
  assert.deepEqual(
    localArgs([
      "extension",
      "install",
      "/tmp/space and ; literal.ts",
      "--options",
      "{}",
    ]),
    ["extension", "install", "/tmp/space and ; literal.ts", "--options", "{}"],
  );
});
test("IPC bounds reject oversized arguments and invalid calls", () => {
  assert.throws(() => bounded("x".repeat(100), 64));
  assert.throws(() =>
    request.parse({
      connection: "workspace",
      method: "info",
      args: Array(9).fill(null),
    }),
  );
});
