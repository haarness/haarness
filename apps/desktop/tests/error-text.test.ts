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
