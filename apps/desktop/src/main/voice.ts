import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { createInterface } from "node:readline";
import type { VoiceEvent } from "../shared";
import { z } from "zod";
const eventSchema = z.discriminatedUnion("type", [
  z.object({ type: z.literal("preparing"), message: z.string() }),
  z.object({ type: z.literal("started") }),
  z.object({
    type: z.literal("transcript"),
    text: z.string(),
    isFinal: z.boolean(),
  }),
  z.object({ type: z.literal("ended") }),
  z.object({ type: z.literal("error"), message: z.string() }),
]);

/** One microphone owner, with per-run identity so stale events cannot reach another draft. */
export function createVoiceInput(
  launch: (locale: string) => ChildProcessWithoutNullStreams,
) {
  let current:
    | {
        owner: number;
        id: string;
        child: ChildProcessWithoutNullStreams;
        emit: (value: VoiceEvent) => void;
        timer?: ReturnType<typeof setTimeout>;
      }
    | undefined;
  function stop(owner?: number, id?: string, cancel = true) {
    const session = current;
    if (
      !session ||
      (owner !== undefined && session.owner !== owner) ||
      (id && session.id !== id)
    )
      return;
    if (session.timer) clearTimeout(session.timer);
    session.child.stdin.write(cancel ? "cancel\n" : "stop\n");
    if (cancel) {
      current = undefined;
      session.emit({ type: "ended" });
      session.child.kill();
    } else {
      session.timer = setTimeout(() => {
        if (current !== session) return;
        current = undefined;
        session.child.kill();
        session.emit({ type: "ended" });
      }, 10000);
    }
  }
  function start(
    owner: number,
    id: string,
    locale: string,
    emit: (value: VoiceEvent) => void,
  ) {
    stop();
    const child = launch(locale);
    const session = { owner, id, child, emit } as NonNullable<typeof current>;
    current = session;
    let ended = false;
    const finish = (error?: string) => {
      if (ended) return;
      ended = true;
      clearTimeout(session.timer);
      if (current !== session) return;
      current = undefined;
      if (error) emit({ type: "error", message: error });
      emit({ type: "ended" });
    };
    child.stdin.on("error", () => {}); // Process exit can race an explicit stop.
    child.stderr.resume(); // Diagnostics never contain transcript text in UI logs.
    const lines = createInterface({ input: child.stdout });
    lines.on("line", (line) => {
      if (current !== session) return;
      try {
        const event = eventSchema.parse(JSON.parse(line));
        if (event.type === "ended") {
          finish();
          return;
        }
        emit(event);
      } catch {
        finish("Dictation returned an invalid response. Please try again.");
        child.kill();
      }
    });
    child.on("error", () =>
      finish(
        "Couldn’t start dictation. Reinstall or rebuild Haarness and try again.",
      ),
    );
    child.on("close", (code) => {
      lines.close();
      finish(
        code && !session.timer
          ? "Dictation stopped unexpectedly. Check microphone and speech permissions in macOS Settings."
          : undefined,
      );
    });
  }
  return { start, stop };
}
export const launchVoice = (executable: string, locale: string) =>
  spawn(executable, ["--locale", locale], { stdio: ["pipe", "pipe", "pipe"] });
