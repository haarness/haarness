import { test } from "node:test";
import assert from "node:assert/strict";
import { mcpInput, newMcpDraft } from "../src/renderer/mcp-form";

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
