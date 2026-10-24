/**
 * A chat without a project works in a folder of its own inside the chat home
 * ("This computer"). The server makes the folder when the chat is created,
 * from the path Desktop names: the local date it was started, then a short
 * name from its first message, so the folders read in Finder as a dated list
 * of what each chat was about. A name already taken gets `-2`, `-3`… there.
 */

/** Letters that have no plain-ASCII decomposition but an obvious spelling. */
const SPELLED: Record<string, string> = {
  ß: "ss",
  æ: "ae",
  œ: "oe",
  ø: "o",
  ł: "l",
  đ: "d",
  ð: "d",
  þ: "th",
  ı: "i",
};
/** Names Windows reserves for devices, which no folder can take. */
const RESERVED = /^(con|prn|aux|nul|com[0-9]|lpt[0-9])$/;
const MAX_WORDS = 6;
const MAX_LENGTH = 40;

/**
 * Up to six words of `text`, folded to plain lowercase ASCII and joined with
 * dashes, in at most 40 characters ("" when nothing of it is ASCII).
 */
export function chatSlug(text: string) {
  const words = text
    .toLowerCase()
    .replace(/[^\u0000-\u007f]/g, (letter) => SPELLED[letter] ?? letter)
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .split(/[^a-z0-9]+/)
    .filter(Boolean)
    .slice(0, MAX_WORDS);
  let slug = "";
  for (const word of words) {
    const next = slug ? slug + "-" + word : word;
    if (next.length > MAX_LENGTH) {
      // A first word longer than the limit is cut; later ones are left out.
      if (!slug) slug = word.slice(0, MAX_LENGTH);
      break;
    }
    slug = next;
  }
  return RESERVED.test(slug) ? "" : slug;
}

/** `date` as the folder above a chat's: its local YYYY-MM-DD. */
export function chatDay(date: Date) {
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/**
 * The folder, relative to the chat home, for a chat whose first message is
 * `text`: `<YYYY-MM-DD>/<slug>`, or `<YYYY-MM-DD>/chat-<4 letters>` when the
 * message gives no name (only attachments, or no Latin letters at all).
 */
export function newChatFolder(
  text: string,
  date = new Date(),
  token = () => crypto.randomUUID().replace(/-/g, "").slice(0, 4),
) {
  return chatDay(date) + "/" + (chatSlug(text) || "chat-" + token());
}

/**
 * Where the chat works relative to the chat home `workspace`, with `/`
 * separators, from its absolute `directory`; undefined when it is not inside.
 */
export function chatFolderPath(workspace: string, directory?: string | null) {
  if (!directory) return undefined;
  const slash = (value: string) => value.replace(/\\/g, "/").replace(/\/+$/, "");
  const root = slash(workspace) + "/";
  const path = slash(directory);
  return path.startsWith(root) && path.length > root.length
    ? path.slice(root.length)
    : undefined;
}

/** The folder's own name, as Finder shows it. */
export const folderName = (directory: string) =>
  directory.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? directory;
