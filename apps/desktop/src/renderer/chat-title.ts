import { t } from "./i18n";

/**
 * A chat's name as Desktop shows it. Until a title is generated the server
 * stores "New thread", which is its word for a chat, not the reader's.
 */
export function chatTitle(title: string | null | undefined) {
  return title && title !== "New thread" ? title : t("Untitled chat");
}
