import { z } from "zod";
export const request = z.object({
  connection: z.string().max(300),
  method: z.string().max(80),
  args: z.array(z.unknown()).max(8),
});
export function bounded(value: unknown, limit = 16 * 1024 * 1024) {
  if (JSON.stringify(value)?.length > limit)
    throw new Error("Request exceeds the desktop message limit");
}
export function external(value: string) {
  const url = new URL(value);
  if (!["https:", "http:", "mailto:"].includes(url.protocol))
    throw new Error("Unsupported external link");
  if (url.username || url.password)
    throw new Error("Links must not contain credentials");
  if (url.protocol === "mailto:") return mailto(url);
  return url.href;
}
/** Fields a mail link may fill in. Some mail apps also honour `attach=`, which would attach a local file. */
const MAIL_FIELDS = new Set(["to", "cc", "bcc", "subject", "body"]);
function mailto(url: URL) {
  const at = url.href.indexOf("?");
  if (at < 0) return url.href;
  const kept = url.href
    .slice(at + 1)
    .split("&")
    .filter((field) => {
      const name = field.split("=")[0];
      try {
        return MAIL_FIELDS.has(decodeURIComponent(name).toLowerCase());
      } catch {
        return false;
      }
    });
  return url.href.slice(0, at) + (kept.length ? "?" + kept.join("&") : "");
}
export function localArgs(args: string[]) {
  const groups = new Set([
    "extension",
    "provider",
    "auth",
    "mcp",
    "agent",
    "command",
    "skill",
    "plugin",
    "schedule",
    "profile",
    "logs",
  ]);
  if (
    !Array.isArray(args) ||
    !groups.has(args[0]) ||
    args.length > 100 ||
    args.some(
      (x) => typeof x !== "string" || x.length > 100000 || x.includes("\0"),
    )
  )
    throw new Error("Unsupported desktop command");
  const denied = [
    "--workspace",
    "-w",
    "-s",
    "--trust-project",
    "--disable-project-config",
    "--chat-home",
    "--data-dir",
    "--yolo",
    "--dangerously-skip-permissions",
    "--thread",
    "--session",
    "--key",
    "--token",
    "--show-secrets",
  ];
  if (
    args.some((arg) =>
      denied.some((flag) => arg === flag || arg.startsWith(flag + "=")),
    )
  )
    throw new Error("This option is managed by the desktop");
  if (args[0] === "auth" && !["login", "logout", "list"].includes(args[1]))
    throw new Error("Use the credential form to store a key");
  return args;
}
