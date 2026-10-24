import { execFileSync } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { join } from "node:path";

/** Where Desktop keeps the chats started without a project ("This
 * computer"): `Chat folders` in its data folder. */
export const chatHomeFolder = (desktopData: string) =>
  join(desktopData, "Chat folders");

/**
 * Stops the chat home's daemon, which Desktop starts on a first launch (its
 * ready message box needs no project), as a test stops its project's. Safe
 * when Desktop never made one.
 */
export function stopChatHome(binary: string, env: Record<string, string | undefined>) {
  const data = env.HAARNESS_DESKTOP_DATA;
  if (!data) return;
  const folder = chatHomeFolder(data);
  if (!existsSync(folder)) return;
  try {
    // Named as the chat home every time, as Desktop names it.
    execFileSync(binary, ["--workspace", realpathSync(folder), "--chat-home", "serve", "--stop"], {
      env: env as NodeJS.ProcessEnv,
      stdio: "ignore",
      timeout: 30000,
    });
  } catch {
    // Already stopped.
  }
}
