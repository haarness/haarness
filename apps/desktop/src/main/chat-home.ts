import { app } from "electron";
import { mkdir, realpath } from "node:fs/promises";
import { join } from "node:path";

/**
 * The chat home: the one folder Haarness owns for chats started without a
 * project ("This computer" in the sidebar). One daemon serves it, and each
 * of its chats works in a folder of its own inside it, which the server
 * creates with the chat. It lives in app data, never in Documents, which a
 * cloud drive may sync. Its name is never "Chats": on a case-insensitive
 * disk that is the older general-chat folder, which stays an ordinary
 * project.
 */
export const CHAT_HOME_FOLDER = "Chat folders";

let resolved: string | undefined;

async function resolve(create: boolean) {
  const folder = join(app.getPath("userData"), CHAT_HOME_FOLDER);
  if (create) await mkdir(folder, { recursive: true });
  try {
    return (resolved = await realpath(folder));
  } catch {
    return folder;
  }
}

/** The chat home's folder, created when it is missing. */
export const chatHomeFolder = () => resolve(true);

/** Whether `workspace` (a resolved path) is the chat home. */
export async function isChatHome(workspace: string) {
  return workspace === (resolved ?? (await resolve(false)));
}
