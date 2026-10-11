import test from "node:test";
import assert from "node:assert/strict";
import { errorText } from "../src/renderer/error-text";

test("server layer prefixes never reach the reader", () => {
  assert.deepEqual(errorText(new Error("tool error: /tmp/does-not-exist does not exist")), {
    message: "/tmp/does-not-exist does not exist",
    known: false,
  });
  assert.equal(errorText("invalid request: tool error: name is required").message, "Name is required");
  // Validation copy the renderer wrote itself passes through unchanged.
  assert.deepEqual(errorText(new Error("Enter a title.")), { message: "Enter a title.", known: false });
});

test("a failure with its reason reads as a title over the reason", () => {
  const text = errorText(
    "tool error: OpenAI sign-in failed: another sign-in is using port 1455 on this computer. Finish or close it, then try again.",
  );
  assert.equal(text.title, "OpenAI sign-in failed");
  assert.equal(
    text.message,
    "Another sign-in is using port 1455 on this computer. Finish or close it, then try again.",
  );
});

test("busy ports and transport failures get plain copy and keep the raw text as detail", () => {
  const port = errorText(
    "tool error: OpenAI sign-in failed: could not listen on localhost:1455: Address already in use (os error 48)",
  );
  assert.equal(port.known, true);
  assert.equal(port.title, "Sign-in could not start");
  assert.match(port.detail!, /^OpenAI sign-in failed: could not listen/);
  assert.equal(errorText("bind: Address already in use").title, "A port is busy");
  const offline = errorText(new Error("tool error: error sending request"));
  assert.deepEqual(
    [offline.known, offline.title, offline.message],
    [true, "Can't connect", "Check your internet connection and try again."],
  );
});

test("only an ordinary first word takes a capital", () => {
  assert.equal(errorText("tool error: gpt-5 is not available for this key").message, "gpt-5 is not available for this key");
  assert.equal(errorText("invalid request: config.toml has an unknown key").message, "config.toml has an unknown key");
  assert.equal(errorText("./scripts/setup.sh exited with status 1").message, "./scripts/setup.sh exited with status 1");
  assert.equal(errorText("v2 of the schema is required").message, "v2 of the schema is required");
  assert.equal(errorText("apiKey is missing").message, "apiKey is missing");
  assert.equal(errorText("e.g. a model id").message, "e.g. a model id");
  assert.equal(errorText("can't reach the server").message, "Can't reach the server");
  assert.equal(errorText("unknown: model claude-x").message, "Unknown: model claude-x");
  assert.equal(errorText("échec de la requête").message, "Échec de la requête");
  assert.equal(errorText("Model sign-in failed: gpt-5.1 needs a paid plan").message, "gpt-5.1 needs a paid plan");
});
