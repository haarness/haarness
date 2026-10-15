import test from "node:test";
import assert from "node:assert/strict";
import {
  isLive,
  parseShare,
  qrPath,
  remoteAction,
  remoteCommand,
  shareOrder,
} from "../src/renderer/remote-control";

test("/remote-control and its short form choose start, status or stop", () => {
  for (const text of ["/remote-control", "/rc", "/RC on", "/rc   share"])
    assert.equal(remoteAction(remoteCommand.exec(text)?.[2] ?? ""), "start", text);
  assert.equal(remoteAction(remoteCommand.exec("/rc stop")![2]), "stop");
  assert.equal(remoteAction(remoteCommand.exec("/remote-control off")![2]), "stop");
  assert.equal(remoteAction(remoteCommand.exec("/remote-control status")![2]), "view");
  assert.equal(remoteAction("later"), null);
  // Other commands that merely start alike are not remote control.
  for (const text of ["/rcx", "/dc", "/remote", "/remote-controls", "rc"])
    assert.equal(remoteCommand.exec(text), null, text);
});

test("a server answer is checked before the dialog trusts it", () => {
  const share = parseShare({
    data: {
      state: "on",
      url: "https://app.haarness.ai/app/remote/sessions/1",
      computer: "Desk",
      chat: "Release",
      error: null,
      qr: ["101", "010", "1x1", 7],
    },
  });
  assert.deepEqual(share, {
    state: "on",
    url: "https://app.haarness.ai/app/remote/sessions/1",
    computer: "Desk",
    chat: "Release",
    error: undefined,
    qr: ["101", "010"],
  });
  assert.equal(isLive(share), true);
  assert.equal(isLive(parseShare({ data: { state: "connecting" } })), true);
  for (const state of ["off", "stopped", "signed_out", "surprise"])
    assert.equal(isLive(parseShare({ data: { state } })), false, state);
  assert.equal(parseShare(null).state, "off");
});

test("QR modules become one path of horizontal runs", () => {
  assert.equal(qrPath(["110", "011", "000"]), "M0 0h2v1h-2zM1 1h2v1h-2z");
  assert.equal(qrPath([]), "");
});

test("a share status answer that was overtaken does not put back an older state", () => {
  const order = shareOrder();
  const first = order.begin("chat");
  const second = order.begin("chat");
  // Two reads, as after a quick reconnect: only the later one counts.
  assert.equal(order.latest("chat", first), false);
  assert.equal(order.latest("chat", second), true);
  // A start finishing in the dialog writes after a read began: the read's answer is dropped.
  const read = order.begin("chat");
  order.begin("chat");
  assert.equal(order.latest("chat", read), false);
  // Chats are ordered apart.
  assert.equal(order.latest("other", order.begin("other")), true);
});
