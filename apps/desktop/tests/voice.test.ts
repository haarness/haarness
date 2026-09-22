import { test } from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { PassThrough } from "node:stream";
import type { ChildProcessWithoutNullStreams } from "node:child_process";
import { createVoiceInput } from "../src/main/voice";
import { insertDictation } from "../src/renderer/dictation";
import type { VoiceEvent } from "../src/shared";

test("dictation replaces the selected range and revises partial results without duplicating words", () => {
  const first = insertDictation("Before [replace] after", 7, 16, "hello");
  assert.equal(first.text, "Before hello after");
  assert.equal(first.caret, 12);
  assert.equal(
    insertDictation("Before [replace] after", 7, 16, "hello world").text,
    "Before hello world after",
  );
  assert.equal(insertDictation("Hi.", 2, 2, "there").text, "Hi there.");
  assert.equal(insertDictation("Keep this", 4, 9, "").text, "Keep this");
});

test("native voice ownership rejects stale events and other-window stop requests", async () => {
  const children: any[] = [];
  const voice = createVoiceInput(() => {
    const child = Object.assign(new EventEmitter(), {
      stdin: new PassThrough(),
      stdout: new PassThrough(),
      stderr: new PassThrough(),
      killed: false,
      kill() {
        this.killed = true;
      },
    });
    children.push(child);
    return child as unknown as ChildProcessWithoutNullStreams;
  });
  const first: VoiceEvent[] = [],
    second: VoiceEvent[] = [];
  voice.start(1, "old", "en-US", (value) => first.push(value));
  children[0].stdout.write(
    '{"type":"preparing","message":"Downloading speech model…"}\n',
  );
  children[0].stdout.write('{"type":"started"}\n');
  voice.start(2, "new", "en-US", (value) => second.push(value));
  assert.equal(children[0].killed, true);
  children[0].stdout.write(
    '{"type":"transcript","text":"wrong chat","isFinal":true}\n',
  );
  voice.stop(1, "old");
  assert.equal(children[1].killed, false);
  children[1].stdout.write(
    '{"type":"transcript","text":"new chat","isFinal":true}\n',
  );
  assert.deepEqual(first, [
    { type: "preparing", message: "Downloading speech model…" },
    { type: "started" },
    { type: "ended" },
  ]);
  assert.deepEqual(second, [
    { type: "transcript", text: "new chat", isFinal: true },
  ]);
  voice.stop(2, "new");
  assert.equal(children[1].killed, true);
  children.forEach((child) => child.emit("close", 0));
});
