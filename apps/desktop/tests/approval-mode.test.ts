import { test } from "node:test";
import assert from "node:assert/strict";
import type { PendingPermissionInfo, Thread } from "@haarness/sdk";
import {
  autoApproval,
  approveAndEnable,
  canAutoApprove,
  createAutoApprover,
  describeAlwaysRule,
  localApprovalMode,
  modeCopy,
  MODES,
  nextMode,
  offeredModes,
  readApprovalModes,
  readMode,
  savedApprovalModes,
  YOLO_CAPABILITY,
  type Mode,
} from "../src/renderer/approval-mode";
const request = (id: string, threadId = "root"): PendingPermissionInfo => ({
  permissionId: id,
  permission: "bash",
  operation: "bash:printf test",
  patterns: ["printf test"],
  alwaysPatterns: ["bash:printf *"],
  detail: {},
  threadId,
  rootThreadId: "root",
  createdAt: new Date().toISOString(),
});
test("Auto scopes by checkout and conversation; child agents inherit but forks do not", () => {
  const modes = readApprovalModes({
    a: { "*": false, root: true },
    b: { "*": true, manual: false },
  });
  const threads = [
    { id: "child", parentId: "root", isSubagent: true },
    { id: "fork", parentId: "root", isSubagent: false },
  ] as Thread[];
  assert.equal(autoApproval(modes, "a", "child", threads), true);
  assert.equal(autoApproval(modes, "a", "fork", threads), false);
  assert.equal(autoApproval(modes, "a", "unknown", [], "root"), true);
  assert.equal(autoApproval(modes, "b", "manual"), false);
  assert.equal(autoApproval(modes, "b", "new"), true);
  assert.equal(autoApproval(modes, "other", "root"), false);
  assert.equal(
    autoApproval(
      { a: { root: "auto", child: "review" } },
      "a",
      "child",
      threads,
      "root",
    ),
    false,
  );
  assert.deepEqual(
    readApprovalModes({ a: { "*": true, bad: "true" }, bad: [], null: null }),
    { a: { "*": "auto" } },
  );
});

test("saved choices read old booleans and Yolo, and save what an older Desktop reads", () => {
  const read = readApprovalModes({
    a: { "*": true, review: false, yolo: "yolo", auto: "auto", plain: "review", newer: "paranoid", n: 1 },
  });
  assert.deepEqual(read, { a: { "*": "auto", review: "review", yolo: "yolo", auto: "auto", plain: "review" } });
  // Auto and Review stay booleans on disk; only Yolo is a word.
  assert.deepEqual(savedApprovalModes(read), {
    a: { "*": true, review: false, yolo: "yolo", auto: true, plain: false },
  });
  assert.deepEqual(readApprovalModes(savedApprovalModes(read)), read);
  // On a server that does not keep modes, this window answers in any mode but Review.
  assert.equal(autoApproval({ a: { "*": "yolo" } }, "a", "new"), true);
  assert.equal(localApprovalMode({ a: { "*": "yolo" } }, "a", "new"), "yolo");
  assert.equal(localApprovalMode({}, "a", "new"), "review");
});

test("modes read safely, cycle Review, Auto, Yolo, and Yolo is offered only where the server has it", () => {
  assert.equal(readMode("auto"), "auto");
  assert.equal(readMode("yolo"), "yolo");
  assert.equal(readMode("review"), "review");
  // A newer server's mode, or none, answers nothing.
  for (const value of ["paranoid", undefined, null, 3, true]) assert.equal(readMode(value), "review");
  const cycle = (yolo: boolean) => {
    const seen: string[] = ["review"];
    let mode: Mode = "review";
    for (let step = 0; step < 3; step++) seen.push((mode = nextMode(mode, yolo)));
    return seen;
  };
  assert.deepEqual(cycle(true), ["review", "auto", "yolo", "review"]);
  assert.deepEqual(cycle(false), ["review", "auto", "review", "auto"]);
  assert.equal(nextMode("yolo", false), "review");
  assert.deepEqual(MODES, ["review", "auto", "yolo"]);
  assert.deepEqual(offeredModes(true), ["review", "auto", "yolo"]);
  assert.deepEqual(offeredModes(false), ["review", "auto"]);
  // A chat in Yolo shows its row even where the server stopped offering it.
  assert.deepEqual(offeredModes(false, "yolo"), ["review", "auto", "yolo"]);
  assert.equal(YOLO_CAPABILITY, "approval-mode.yolo.v1");
  assert.deepEqual(
    MODES.map((mode) => [modeCopy[mode].name, modeCopy[mode].detail]),
    [
      ["Review", "Asks before risky steps"],
      ["Auto", "Answers safe requests without asking"],
      ["Yolo", "Approves every request without asking"],
    ],
  );
});
test("Auto leaves restricted requests manual", () => {
  for (const permission of ["instruction_import"])
    assert.equal(canAutoApprove({ ...request("1"), permission }), false);
  assert.equal(canAutoApprove({ ...request("1"), allowOnce: false }), false);
  assert.equal(canAutoApprove({ ...request("1"), allowAlways: false }), false);
  assert.equal(canAutoApprove(request("1")), true);
});
test("Auto attempts each permission once across replay, keeps failures manual, and stops in Review", async () => {
  let enabled = true;
  const replies: string[] = [],
    failed: string[] = [];
  const consume = createAutoApprover({
    enabled: () => enabled,
    reply: async (connection, id) => {
      replies.push(connection + ":" + id);
      if (id === "fail") throw Error("offline");
    },
    failed: (_, r) => failed.push(r.permissionId),
  });
  consume("a", [request("ok"), request("fail")], []);
  consume("a", [request("ok"), request("fail")], []);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(replies, ["a:ok", "a:fail"]);
  assert.deepEqual(failed, ["fail"]);
  enabled = false;
  consume("a", [request("new")], []);
  assert.equal(replies.length, 2);
  enabled = true;
  consume("b", [request("ok")], []);
  assert.equal(replies.at(-1), "b:ok");
});

test("Allow once, then Auto acknowledges the current prompt before enabling the consumer", () => {
  const replies: string[] = [];
  const consume = createAutoApprover({
    enabled: () => true,
    reply: async (_, id) => {
      replies.push(id);
    },
    failed: () => assert.fail("unexpected retry"),
  });
  consume.acknowledge("a", "current");
  consume("a", [request("current"), request("next")], []);
  assert.deepEqual(replies, ["next"]);
});

test("Allow once, then Auto never enables after a failed reply or after navigation/review changes", async () => {
  let enabled = 0,
    acknowledged = 0,
    current = true;
  const callbacks = {
    acknowledge: () => {
      acknowledged++;
    },
    current: () => current,
    enable: async () => {
      enabled++;
    },
  };
  await assert.rejects(
    approveAndEnable({
      ...callbacks,
      reply: async () => {
        throw Error("offline");
      },
    }),
  );
  assert.equal(enabled, 0);
  assert.equal(acknowledged, 0);
  let resolve!: () => void;
  const reply = new Promise<void>((done) => {
    resolve = done;
  });
  const pending = approveAndEnable({ ...callbacks, reply: () => reply });
  current = false;
  resolve();
  await pending;
  assert.equal(enabled, 0);
  assert.equal(acknowledged, 1);
  current = true;
  assert.equal(await approveAndEnable({ ...callbacks, reply: async () => {} }), false);
  assert.equal(enabled, 1);
  // Answered already (Auto's reviewer, another window): Auto still turns on,
  // and the caller hears the request was settled.
  const { alreadySettled } = await import("../src/renderer/approval-mode");
  assert.equal(
    await approveAndEnable({
      ...callbacks,
      settled: alreadySettled,
      reply: async () => {
        throw Error("not found: permission 0199");
      },
    }),
    true,
  );
  assert.equal(enabled, 2);
  // Any other failure still enables nothing.
  await assert.rejects(
    approveAndEnable({
      ...callbacks,
      settled: alreadySettled,
      reply: async () => {
        throw Error("offline");
      },
    }),
  );
  assert.equal(enabled, 2);
});

test("always rules read as plain words, never as stored permission patterns", () => {
  const words = {
    t: (key: string, values: Record<string, string> = {}) =>
      key.replace(/\{(\w+)\}/g, (_, name: string) => values[name] ?? ""),
    shortPath: (path: string) => "…/" + path.split("/").slice(-2).join("/"),
  };
  const say = (permission: string, rule: string) => describeAlwaysRule(permission, rule, words);
  assert.equal(
    say("external_directory", "external_directory:/private/tmp/qa/desktop-onboarding/projects/*"),
    "Anything in …/desktop-onboarding/projects",
  );
  assert.equal(say("external_directory", "external_directory:*"), "Any folder outside the project");
  assert.equal(say("bash", "bash:git push *"), "Commands starting with git push");
  assert.equal(say("bash", "bash:*"), "Every command");
  assert.equal(say("edit", "edit:*"), "Every file edit");
  assert.equal(say("read", "read:.env"), "Reading .env");
  assert.equal(say("media_generate", "media_generate:image:*"), "Any image generation");
  assert.equal(say("agent_switch", "agent_switch:thread:6275696c64"), "Only this agent, in this chat");
  assert.equal(say("glob", "glob:*"), "Every request of this kind");
  // Without a plainer reading the rule is still shown, minus its prefix.
  assert.equal(say("browser_site", "browser_site:https://example.com"), "https://example.com");
  assert.equal(say("multiple", "browser:navigate:https://example.com"), "browser:navigate:https://example.com");
});

test("the reviewer's note reads as one quiet line, and only for a reviewed request", async () => {
  const { reviewNote } = await import("../src/renderer/approval-mode");
  const t = (key: string) => key;
  assert.equal(reviewNote(undefined, t), "");
  assert.equal(reviewNote(null, t), "");
  assert.equal(
    reviewNote(
      {
        status: "asked",
        risk: "high",
        rule: "Shared state",
        rationale: "It pushes to main, which your messages do not ask for.",
      },
      t,
    ),
    "Auto review · Shared state: It pushes to main, which your messages do not ask for.",
  );
  assert.equal(
    reviewNote({ status: "failed", rationale: " It took longer than 60 seconds. " }, t),
    "Auto review unavailable: It took longer than 60 seconds.",
  );
  assert.equal(reviewNote({ status: "paused" }, t), "Auto review paused");
  // Haarness's own notes are worded by their code, in the person's language.
  const words = (key: string, values: Record<string, string | number> = {}) =>
    "«" + key.replace(/\{(\w+)\}/g, (_, name) => String(values[name])) + "»";
  assert.equal(
    reviewNote({ status: "failed", reason: { code: "timeout", seconds: 60 }, rationale: "English" }, words),
    "«Auto review unavailable»: «It took more than 60 s.»",
  );
  assert.equal(
    reviewNote({ status: "paused", reason: { code: "streak", count: 3 }, rationale: "English" }, words),
    "«Auto review paused»: «It blocked 3 actions in a row, so the rest of this turn is yours to approve.»",
  );
  // Haarness's own rule is translated too, and a path or an error keeps its
  // own direction in the sentence (FSI … PDI).
  assert.equal(
    reviewNote(
      { status: "asked", rule: "Destructive action", reason: { code: "unnamed_deletion", path: "/tmp/x" } },
      words,
    ),
    "«Auto review» · «Destructive action»: «This deletes \u2068/tmp/x\u2069, outside the project, and your messages do not name it.»",
  );
  assert.equal(
    reviewNote(
      { status: "asked", rule: "Destructive action", reason: { code: "unplaced_deletion", path: "\"$TARGET\"" } },
      words,
    ),
    "«Auto review» · «Destructive action»: «This deletes \u2068\"$TARGET\"\u2069, a path Haarness can't place, and your messages do not name it.»",
  );
  assert.equal(
    reviewNote({ status: "asked", reason: { code: "unnamed_overwrite", path: "/etc/hosts" } }, words),
    "«Auto review»: «This overwrites \u2068/etc/hosts\u2069, outside the project, and your messages do not name it.»",
  );
  assert.equal(
    reviewNote({ status: "asked", reason: { code: "unread_script", program: "bash" } }, words),
    "«Auto review»: «This runs a here-document Haarness could not read with \u2068bash\u2069, and your messages do not ask for it.»",
  );
  assert.equal(
    reviewNote({ status: "asked", rule: "Destructive action", reason: { code: "inline_code", program: "python3 -c" } }, words),
    "«Auto review» · «Destructive action»: «This runs \u2068python3 -c\u2069 code that deletes files or reaches outside the project, and your messages do not ask for it.»",
  );
  assert.equal(
    reviewNote({ status: "failed", reason: { code: "error", detail: "HTTP 500" } }, words),
    "«Auto review unavailable»: «The review failed (\u2068HTTP 500\u2069).»",
  );
  // The reviewer's own rule is shown as it wrote it.
  assert.equal(
    reviewNote({ status: "asked", rule: "Shared state", rationale: "Pushes to main." }, words),
    "«Auto review» · Shared state: Pushes to main.",
  );
  const { reviewNoteParts } = await import("../src/renderer/approval-mode");
  assert.deepEqual(
    reviewNoteParts({ status: "asked", rule: "Shared state", rationale: "Pushes to main." }, t),
    { lead: "Auto review", rule: "Shared state", reason: "Pushes to main." },
  );
  assert.deepEqual(reviewNoteParts({ status: "paused" }, t), {
    lead: "Auto review paused",
    rule: undefined,
    reason: undefined,
  });
  assert.equal(
    reviewNote({ status: "asked", risk: "critical", reason: { code: "rated" } }, words),
    "«Auto review»: «It rated this action critical risk.»",
  );
  // A code from a newer server shows its English.
  assert.equal(
    reviewNote({ status: "asked", reason: { code: "unknown" }, rationale: "As written." }, words),
    "«Auto review»: As written.",
  );
  assert.equal(reviewNote({ status: "reviewing" }, words), "«Auto review is deciding…»");
});

test("a reply the server no longer knows was settled already", async () => {
  const { alreadySettled } = await import("../src/renderer/approval-mode");
  assert.equal(alreadySettled(new Error("not found: permission 0199")), true);
  assert.equal(alreadySettled(new Error("conflict: busy")), false);
});

test("the pill's tip says what its mode does, and that Auto asks a reviewer only where the server's does", async () => {
  const { approvalModeHint } = await import("../src/renderer/approval-mode");
  const t = (key: string) => key;
  assert.match(approvalModeHint("auto", true, true, t), /a reviewer model approves routine permission requests in this chat/);
  assert.match(approvalModeHint("auto", false, true, t), /in new chats here once, and asks you about the rest/);
  // auto_review.enabled false, or an older server: no approval-review.v1.
  assert.equal(
    approvalModeHint("auto", true, false, t),
    "Auto: permission requests in this chat and its agents are allowed once without asking.",
  );
  assert.match(approvalModeHint("review", true, true, t), /^Review: .* this chat and its agents ask before/);
  assert.match(approvalModeHint("review", false, false, t), /new chats here ask before/);
  assert.equal(
    approvalModeHint("yolo", true, true, t),
    "Yolo: every permission request in this chat and its agents is approved once without asking. Deny rules still hold, and requests only you can answer still ask.",
  );
  assert.match(approvalModeHint("yolo", false, false, t), /^Yolo: every permission request in new chats here/);
  // A menu, not a toggle: no tip tells the reader to click to switch.
  for (const mode of MODES)
    for (const chat of [true, false]) assert.doesNotMatch(approvalModeHint(mode, chat, true, t), /Click/);
});

test("new chats start in the mode chosen for them here, else in the server's default", async () => {
  const { newChatMode } = await import("../src/renderer/approval-mode");
  // A server that does not name its default (an older one) starts them in Auto.
  assert.equal(newChatMode({}, "a"), "auto");
  assert.equal(newChatMode({ a: { root: "review" } }, "a"), "auto");
  assert.equal(newChatMode({ a: { "*": "review" } }, "a"), "review");
  assert.equal(newChatMode({ a: { "*": "review" } }, "b"), "auto");
  // Configured Review, Auto, Yolo, nothing configured (Auto), a newer mode.
  assert.equal(newChatMode({}, "a", "review"), "review");
  assert.equal(newChatMode({ a: { root: "auto" } }, "a", "review"), "review");
  assert.equal(newChatMode({}, "a", "auto"), "auto");
  assert.equal(newChatMode({}, "a", "yolo"), "yolo");
  assert.equal(newChatMode({}, "a", null), "auto");
  assert.equal(newChatMode({}, "a", "paranoid"), "review");
  // The choice made here wins either way, as the chat Desktop starts takes it.
  assert.equal(newChatMode({ a: { "*": "auto" } }, "a", "review"), "auto");
  assert.equal(newChatMode({ a: { "*": "review" } }, "a", "yolo"), "review");
  assert.equal(newChatMode({ a: { "*": "yolo" } }, "a", "review"), "yolo");
  assert.equal(newChatMode({ a: { "*": "auto" } }, "b", "review"), "review");
  // A Yolo chosen here counts only where the server offers it.
  assert.equal(newChatMode({ a: { "*": "yolo" } }, "a", "review", false), "review");
  assert.equal(newChatMode({ a: { "*": "yolo" } }, "a", undefined, false), "auto");
});
