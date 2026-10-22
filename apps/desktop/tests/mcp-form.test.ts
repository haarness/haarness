import { test } from "node:test";
import assert from "node:assert/strict";
import { keychainEntry, keychainInput, mcpInput, newMcpDraft, renameKeychainReference, secretLooking } from "../src/renderer/mcp-form";

test("local MCP form preserves ordered argv and environment values without shell parsing", () => {
  const draft = newMcpDraft();
  Object.assign(draft, {
    name: " tools ",
    command: " /path with spaces/server ",
    scope: "local",
  });
  draft.args = [
    { key: "-y", value: "" },
    { key: "package-name", value: "" },
    { key: "--directory", value: "/project with spaces" },
    { key: "--filter=x=y", value: "" },
    { key: "$(literal)", value: "  preserve  " },
    { key: "", value: "" },
  ];
  draft.environment = [
    { key: " API_KEY ", value: " secret=with spaces " },
    { key: "EMPTY", value: "" },
  ];
  draft.url = "https://inactive.test";
  draft.headers = [{ key: "invalid header", value: "ignored" }];
  assert.deepEqual(mcpInput(draft), {
    action: "add",
    name: "tools",
    type: "local",
    scope: "local",
    enabled: true,
    command: "/path with spaces/server",
    args: [
      "-y",
      "package-name",
      "--directory",
      "/project with spaces",
      "--filter=x=y",
      "$(literal)",
      "  preserve  ",
    ],
    environment: { API_KEY: " secret=with spaces ", EMPTY: "" },
  });
});

test("remote MCP form isolates headers and OAuth from the local draft", () => {
  const draft = newMcpDraft();
  Object.assign(draft, {
    name: "remote",
    type: "remote",
    url: " https://mcp.example.test/tools ",
    enabled: false,
    oauthClientId: " client-id ",
    oauthScopes: "read write read",
  });
  draft.headers = [{ key: "Authorization", value: "Bearer fixture" }];
  draft.environment = [{ key: "", value: "inactive" }];
  assert.deepEqual(mcpInput(draft), {
    action: "add",
    name: "remote",
    type: "remote",
    scope: "global",
    enabled: false,
    url: "https://mcp.example.test/tools",
    headers: { Authorization: "Bearer fixture" },
    oauthClientId: "client-id",
    oauthScopes: ["read", "write"],
  });
  draft.oauth = "disabled";
  const disabled = mcpInput(draft);
  assert.equal(disabled.oauth, false);
  assert.ok(!("oauthClientId" in disabled));
  assert.ok(!("oauthScopes" in disabled));
  draft.oauth = "enabled";
  assert.equal(mcpInput(draft).oauth, true);
});

test("MCP validation rejects duplicate names, malformed targets and incomplete rows without exposing values", () => {
  const draft = newMcpDraft();
  Object.assign(draft, { name: "local", command: "npx" });
  draft.args = [{ key: "", value: "sensitive" }];
  assert.throws(() => mcpInput(draft), /Argument 1 needs/);
  draft.args = [];
  draft.environment = [
    { key: "KEY", value: "secret" },
    { key: "KEY", value: "other" },
  ];
  assert.throws(() => mcpInput(draft), /Environment variable 2 repeats a name/);
  draft.environment = [{ key: "bad name", value: "secret" }];
  assert.throws(() => mcpInput(draft), /invalid name/);
  Object.assign(draft, { type: "remote", url: "file:///tmp/mcp" });
  assert.throws(() => mcpInput(draft), /must use HTTP/);
  draft.url = "https://user:secret@example.test";
  assert.throws(() => mcpInput(draft), /request header/);
  draft.url = "https://example.test";
  draft.headers = [
    { key: "Authorization", value: "secret" },
    { key: "authorization", value: "secret" },
  ];
  assert.throws(() => mcpInput(draft), /Header 2 repeats a name/);
  draft.headers = [
    { key: "Authorization", value: "secret\r\nInjected: value" },
  ];
  assert.throws(
    () => mcpInput(draft),
    /Header 1 contains an invalid character/,
  );
  draft.headers = [{ key: "", value: "secret" }];
  assert.throws(() => mcpInput(draft), /Header 1 needs a name/);
});

test("a secret-looking value offers the keychain, and a kept one is written as a reference", () => {
  const draft = newMcpDraft();
  Object.assign(draft, { name: "git hub", type: "remote", url: "https://mcp.example.test/mcp" });
  draft.headers = [
    { key: "Authorization", value: "Bearer ghp_0123456789abcdefghij", vault: true },
    { key: "X-Api-Key", value: "k-1", vault: true },
    { key: "X-Trace", value: "on", vault: true },
    { key: "X-Token", value: "${TEAM_TOKEN}", vault: true },
    { key: "X-Plain-Token", value: "kept-as-written" },
  ];
  // Named like a credential, or a long run of letters and digits; never a reference or a blank.
  assert.equal(secretLooking(draft.headers[0]), true);
  assert.equal(secretLooking({ key: "NOTE", value: "a1b2c3d4e5f6g7h8i9j0k1l2m3" }), true);
  assert.equal(secretLooking(draft.headers[2]), false);
  assert.equal(secretLooking(draft.headers[3]), false);
  assert.equal(secretLooking({ key: "API_KEY", value: "  " }), false);
  const { input, secrets } = keychainInput(draft);
  // The scheme stays written, so the file still says what the header is.
  assert.deepEqual(input.headers, {
    Authorization: "Bearer ${keychain:git-hub-Authorization}",
    "X-Api-Key": "${keychain:git-hub-X-Api-Key}",
    "X-Trace": "on",
    "X-Token": "${TEAM_TOKEN}",
    "X-Plain-Token": "kept-as-written",
  });
  assert.deepEqual(secrets, [
    { name: "git-hub-Authorization", secret: "ghp_0123456789abcdefghij" },
    { name: "git-hub-X-Api-Key", secret: "k-1" },
  ]);
  assert.equal(keychainEntry("a/b", "TOKEN"), "a-b-TOKEN");
  // An entry name starts with a letter or digit, whatever the server is called.
  assert.equal(keychainEntry("_hidden", "TOKEN"), "hidden-TOKEN");
  assert.equal(keychainEntry("日本", "TOKEN"), "TOKEN");
  assert.equal(keychainEntry("日本", "_"), "mcp-secret");
  // When the keychain gives another name (the first was taken), the reference follows it.
  renameKeychainReference(input, "git-hub-Authorization", "git-hub-Authorization-2");
  assert.equal((input.headers as Record<string, string>).Authorization, "Bearer ${keychain:git-hub-Authorization-2}");
  assert.equal((input.headers as Record<string, string>)["X-Api-Key"], "${keychain:git-hub-X-Api-Key}");
  // A local server keeps its environment the same way.
  const local = newMcpDraft();
  Object.assign(local, { name: "files", command: "server" });
  local.environment = [{ key: "GITHUB_TOKEN", value: "ghp_x", vault: true }];
  assert.deepEqual(keychainInput(local).input.environment, { GITHUB_TOKEN: "${keychain:files-GITHUB_TOKEN}" });
});
