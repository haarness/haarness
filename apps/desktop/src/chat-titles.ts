/**
 * Whether a stored chat title is a name somebody gave the chat. Until a title
 * is generated the server stores a placeholder ("New thread", which is its
 * word for a chat; "New chat" from a server that uses the reader's word), and
 * a fork of such a chat is stored as "Fork of New thread". None of them is a
 * name, so Desktop shows them as an untitled chat or a fork of one.
 *
 * Shared by the renderer and the main process, so it returns a kind rather
 * than translated text.
 */
export function untitledKind(
  title: string | null | undefined,
): "chat" | "fork" | undefined {
  let value = (title ?? "").trim();
  let forks = 0;
  while (/^Fork of(?:\s|$)/.test(value)) {
    value = value.slice("Fork of".length).trim();
    forks++;
  }
  if (value && value !== "New thread" && value !== "New chat") return undefined;
  return forks ? "fork" : "chat";
}
