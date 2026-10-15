/**
 * A `haarness://` link as the system hands it to Desktop:
 * `haarness://open?workspace=<absolute folder>&session=<chat id>`. Values are
 * trimmed. A folder that is not an absolute local path makes the whole link
 * invalid, so a link can never make Desktop resolve a network share. Opening
 * the folder is the main process's decision: a folder already in the sidebar
 * opens as saved, any other is confirmed and opens untrusted.
 */
export interface DeepLink {
  workspace?: string;
  session?: string;
}

export function parseDeepLink(input: string): DeepLink | undefined {
  let url: URL;
  try {
    url = new URL(input.trim());
  } catch {
    return undefined;
  }
  if (url.protocol !== "haarness:") return undefined;
  const value = (name: string) => url.searchParams.get(name)?.trim() || undefined;
  const workspace = value("workspace");
  const session = value("session");
  // `/folder` or `C:\folder`, never `//host`, `\\host` or `\\?\UNC\host`.
  if (workspace && !/^(?:\/(?![\\/])|[a-z]:[\\/](?![\\/]))/i.test(workspace)) return undefined;
  if (session && !/^[\w.:-]{1,200}$/.test(session)) return undefined;
  if (!workspace && !session) return undefined;
  return { ...(workspace ? { workspace } : {}), ...(session ? { session } : {}) };
}
