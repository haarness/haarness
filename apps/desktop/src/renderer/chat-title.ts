import { t } from "./i18n";
import { untitledKind } from "../chat-titles";

/**
 * A chat's name as Desktop shows it. Until a title is generated the server
 * stores the placeholder "New chat" ("New thread" on older servers), which is
 * not a name; a fork of that chat would otherwise read "Fork of New chat".
 */
export function chatTitle(title: string | null | undefined) {
  const kind = untitledKind(title);
  if (kind === "chat") return t("Untitled chat");
  if (kind === "fork") return t("Fork of untitled chat");
  return title!;
}

/** The text a rename starts from: the name as shown, or nothing to replace
 * when the chat has none yet. */
export function editableTitle(title: string | null | undefined) {
  return untitledKind(title) === "chat" ? "" : chatTitle(title);
}
