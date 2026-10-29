import test from "node:test";
import assert from "node:assert/strict";
import { ApiError, TransportError } from "@haarness/sdk";
import { cancelled, conflictAnswer, errorText, errorTextLine, failureDetail, failureMessage, notFoundAnswer } from "../src/renderer/error-text";

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

test("a status without a sentence reads as what to do, in the same words as the phones", () => {
  const bare = (status: number) => new ApiError(status, "http_error", `HTTP ${status}`, false, "");
  assert.deepEqual(errorText(bare(502)), {
    title: "Haarness isn't answering",
    message: "Try again in a moment.",
    detail: "HTTP 502",
    known: true,
  });
  for (const status of [503, 504]) assert.equal(errorText(bare(status)).title, "Haarness isn't answering");
  assert.equal(errorText(bare(500)).message, "Something went wrong on our side. Try again in a moment.");
  assert.equal(errorText(bare(408)).message, "This took too long. Try again in a moment.");
  assert.equal(errorText(bare(429)).message, "Too many requests. Wait a moment, then try again.");
  assert.equal(errorText(bare(403)).message, "You don't have access to this.");
  for (const status of [404, 410]) assert.equal(errorText(bare(status)).message, "This is no longer available. It may have been removed.");
  assert.equal(errorText(bare(413)).message, "This is too large to send.");
  assert.equal(errorText(bare(400)).message, "That didn't go through. Try again.");
  // A wrong token or password says it was refused; through Cloud's relay a
  // 401 is Cloud's sign-in.
  assert.equal(errorText(bare(401)).message, "The server refused the token or password. Check it, then try again.");
  assert.equal(errorText("Unauthorized").message, "The server refused the token or password. Check it, then try again.");
  assert.equal(errorText(bare(401), { relay: true }).message, "That didn't go through. Try again.");
  // A refusal the server words for people is its own, whatever its status.
  assert.deepEqual(errorText(new ApiError(400, "http_error", "Path must be relative to the workspace", false, "Path must be relative to the workspace")), {
    message: "Path must be relative to the workspace",
    known: false,
  });
  // A reason phrase, a status line or a proxy's page is no sentence either.
  for (const said of ["Bad Gateway", "502 Bad Gateway", "bad gateway", "HTTP/1.1 502 Bad Gateway", "<html><body><h1>502 Bad Gateway</h1></body></html>"])
    assert.equal(errorText(new ApiError(502, "http_error", said, false, said)).title, "Haarness isn't answering", said);
  assert.equal(errorText("Gateway Time-out").title, "Haarness isn't answering");
  assert.equal(errorText("Error: HTTP 404").message, "This is no longer available. It may have been removed.");
  // A server's own sentence is still the server's, whatever its status.
  assert.deepEqual(errorText(new ApiError(502, "provider_error", "tool error: the model is overloaded", true, null)), {
    message: "The model is overloaded",
    known: false,
  });
  assert.equal(errorText("Conflict in a.ts: both sides changed it").known, false);
});

test("another computer that could not answer, or cannot be reached, says so", () => {
  const computer = ["Your computer couldn't answer", "Check that Haarness is open and up to date on it, then try again."];
  const said = (error: unknown, relay = false) => {
    const text = errorText(error, { relay });
    return [text.title, text.message];
  };
  assert.deepEqual(said(new ApiError(502, "bad_gateway", "Your computer could not complete this request.", true, null)), computer);
  assert.deepEqual(
    said(new ApiError(502, "project_server_unavailable", "This project's Haarness server is not reachable", true, null)),
    computer,
  );
  // A bare 5xx through Cloud's relay is the computer's, elsewhere Haarness's.
  assert.deepEqual(said(new ApiError(502, "http_error", "HTTP 502", false, ""), true), computer);
  assert.deepEqual(said(new ApiError(504, "http_error", "Gateway Timeout", false, "Gateway Timeout"), true), computer);
  const offline = new ApiError(409, "conflict", "Remote host is offline or its connection changed. Reconnect and check the current state before retrying.", false, null);
  assert.deepEqual(said(offline), ["Your computer isn't reachable", "Open Haarness on it, then try again."]);
  assert.deepEqual(said(new Error(offline.message)), ["Your computer isn't reachable", "Open Haarness on it, then try again."]);
  assert.deepEqual(said(new ApiError(409, "remote_offline", "", false, null)), ["Your computer isn't reachable", "Open Haarness on it, then try again."]);
});

test("transport failures and timeouts get plain copy; the cause is kept for the log", () => {
  const refused = new TransportError("Haarness request failed", {
    cause: new TypeError("fetch failed", { cause: Object.assign(new Error("connect ECONNREFUSED 127.0.0.1:4242"), { code: "ECONNREFUSED" }) }),
  });
  // A server on this computer that takes no connection is Haarness's.
  assert.deepEqual([errorText(refused).title, errorText(refused).message], ["Haarness isn't answering", "Try again in a moment."]);
  assert.match(failureDetail(refused), /TransportError · Haarness request failed · TypeError · fetch failed · .*ECONNREFUSED/);
  const away = new TransportError("Haarness request failed", {
    cause: new TypeError("fetch failed", { cause: Object.assign(new Error("getaddrinfo ENOTFOUND api.haarness.ai"), { code: "ENOTFOUND" }) }),
  });
  assert.deepEqual([errorText(away).title, errorText(away).message], ["Can't connect", "Check your internet connection and try again."]);
  const slow = new TransportError("Haarness request failed", {
    cause: new TypeError("fetch failed", { cause: Object.assign(new Error("Connect Timeout Error"), { code: "UND_ERR_CONNECT_TIMEOUT" }) }),
  });
  const timeout = "This took too long. Check your connection, then try again.";
  assert.equal(errorText(slow).message, timeout);
  assert.equal(errorText(new DOMException("Request timed out", "TimeoutError")).message, timeout);
  assert.equal(errorText(new Error("The operation was aborted due to timeout")).message, timeout);
  assert.equal(errorText(new TransportError("The event stream sent nothing for 150 ms")).message, timeout);
  for (const said of ["fetch failed", "Failed to fetch", "Haarness request failed", "error sending request for url (https://api.haarness.ai/v1/agents)"])
    assert.equal(errorText(said).title, "Can't connect", said);
  assert.equal(errorText("error sending request for url (http://127.0.0.1:4242/v1/agents)").title, "Haarness isn't answering");
  assert.equal(errorText(new TransportError("Server returned invalid JSON")).title, "Haarness isn't answering");
});

test("a cancelled request is no failure", () => {
  for (const error of [
    new DOMException("This operation was aborted", "AbortError"),
    new Error("This operation was aborted"),
    "The operation was aborted.",
    new Error("AbortError: The user aborted a request."),
  ]) {
    assert.equal(cancelled(error), true, String(error));
    assert.deepEqual(errorText(error), { message: "", known: true });
  }
  assert.equal(cancelled(new DOMException("The operation was aborted due to timeout", "TimeoutError")), false);
  assert.equal(cancelled(new Error("Opening was cancelled")), false);
  assert.equal(cancelled(undefined), false);
});

test("main hands the window copy only the error could choose, on one line", () => {
  const relayed = new ApiError(502, "http_error", "HTTP 502", false, "");
  const line = "Your computer couldn't answer. Check that Haarness is open and up to date on it, then try again.";
  assert.equal(failureMessage(relayed, { relay: true }), line);
  // The window reads the line back as its title and message.
  assert.deepEqual(errorText(new Error(line)), {
    title: "Your computer couldn't answer",
    message: "Check that Haarness is open and up to date on it, then try again.",
    known: true,
  });
  assert.equal(errorTextLine(errorText(new Error(line))), line);
  // What the window reads the same way goes as it is, for its tooltip.
  assert.equal(failureMessage(relayed), "HTTP 502");
  assert.equal(failureMessage(new Error("Name is required")), "Name is required");
  assert.equal(
    failureMessage(new TransportError("The side question sent nothing for 300000 ms")),
    "This took too long. Check your connection, then try again.",
  );
  assert.equal(failureMessage(new DOMException("This operation was aborted", "AbortError")), "This operation was aborted");
  assert.equal(errorTextLine(errorText("tool error: Model sign-in failed: gpt-5.1 needs a paid plan")), "Model sign-in failed: gpt-5.1 needs a paid plan");
});

test("only the server's answer that something does not exist reads as not found", () => {
  // The server's own words, a bare status, and what main says for one.
  assert.equal(notFoundAnswer(new Error("not found: command cmd-4f2a")), true);
  assert.equal(notFoundAnswer(new Error("HTTP 404")), true);
  assert.equal(notFoundAnswer(new Error("410 Gone")), true);
  assert.equal(notFoundAnswer(new Error(failureMessage(Object.assign(new Error(""), { name: "ApiError", status: 404 })))), true);
  assert.equal(notFoundAnswer(Object.assign(new Error("x"), { status: 404 })), true);
  // A computer offline through the relay, a workspace asleep, the network.
  assert.equal(notFoundAnswer(new Error("conflict: the computer is offline")), false);
  assert.equal(notFoundAnswer(new Error("HTTP 409")), false);
  assert.equal(notFoundAnswer(new Error("invalid request: the workspace is asleep")), false);
  assert.equal(notFoundAnswer(new Error("fetch failed")), false);
});

test("a refusal that conflicts with how things stand reads as one, and keeps its reason across the window", () => {
  const refused = Object.assign(new Error("conflict: this chat runs as many commands in the background as it may"), {
    name: "ApiError",
    status: 409,
  });
  // Main hands the window the server's words as they were.
  assert.equal(failureMessage(refused), refused.message);
  assert.equal(conflictAnswer(new Error(failureMessage(refused))), true);
  assert.equal(conflictAnswer(new Error("HTTP 409")), true);
  assert.equal(conflictAnswer(new Error("not found: command cmd-1")), false);
  // A computer offline through the relay says so in plain words instead.
  const offline = Object.assign(new Error("computer offline"), { name: "ApiError", status: 409 });
  assert.equal(conflictAnswer(new Error(failureMessage(offline, { relay: true }))), false);
});
