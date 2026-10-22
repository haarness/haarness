/*
 * What a hook's matcher can match, for the hook editor.
 *
 * The server says what each event's matcher is compared with (the catalog's
 * `matchers`); these helpers turn that into suggestions and mirror the
 * runtime's matcher (`crates/haarness-server/src/hooks.rs`), so the editor can
 * say at once when a matcher can never match its event.
 */

/** What an event's matcher is compared with: a tool name, a permission
 * name, one of `values` (the only ones Haarness sends, suggested) or
 * `aliases` (other names it accepts, such as Claude Code's), or nothing at
 * all (the matcher is ignored and the hook always runs). */
export interface MatcherTarget {
  compares: "tool" | "permission" | "value" | "nothing";
  values?: string[];
  aliases?: string[];
}

/** What the matcher field is for: an event's target, or `unknown` for a
 * server from before the catalog said what each event matches, which gets
 * the generic field it always had (no suggestions, no verdict). */
export function matcherKind(target: MatcherTarget | undefined) {
  return target?.compares ?? "unknown";
}

/** A suggestion: the text it puts in the matcher, and a few words on it. */
export interface MatcherOption {
  value: string;
  label?: string;
}

/** What each value an event sends means, in the editor's words. */
const valueLabels: MatcherOption[] = [
  { value: "manual", label: "Compacted with /compact" },
  { value: "automatic", label: "Compacted near the context limit" },
  { value: "authentication_failed", label: "Sign-in or API key rejected" },
  { value: "rate_limit", label: "Rate limit or quota reached" },
  { value: "overloaded", label: "Provider overloaded" },
  { value: "server_error", label: "Provider server error" },
  { value: "invalid_request", label: "Request rejected" },
  { value: "provider", label: "Any provider error" },
  { value: "incomplete", label: "Reply ended early" },
  { value: "content_filter", label: "Stopped by a safety check" },
  { value: "tool", label: "A tool failed" },
  { value: "other", label: "Any other failure" },
  { value: "startup", label: "A chat was created" },
  { value: "archived", label: "A chat was archived" },
  { value: "permission_prompt", label: "A permission prompt is waiting" },
  { value: "turn_completed", label: "A reply finished" },
];

/** The English catalog key describing `value`, when it has one. */
export function valueLabel(value: string) {
  return valueLabels.find((option) => option.value === value)?.label;
}

/** Claude Code's tool names, which matchers accept for Haarness's. */
const claudeAliases: Record<string, string> = {
  Bash: "bash",
  Read: "read",
  Edit: "edit",
  MultiEdit: "edit",
  NotebookEdit: "edit",
  Write: "write",
  WebFetch: "webfetch",
  Glob: "glob",
  Grep: "grep",
  LS: "list",
  Task: "task",
  TodoWrite: "todowrite",
  AskUserQuestion: "question",
};

/** Whether `matcher` matches `value`, as the runtime decides: `a|b`
 * alternatives of plain names (Claude's names accepted), or a regular
 * expression found anywhere in the value. */
export function matcherMatches(matcher: string, value: string) {
  const canonical = value === "todo" ? "todowrite" : value;
  const name = (candidate: string) =>
    candidate.toLowerCase() === "todo" ? "todowrite" : (claudeAliases[candidate] ?? candidate);
  const parts = matcher.split("|").map((part) => part.trim());
  const normalized = parts.map(name).join("|");
  if (/^[A-Za-z0-9_|-]*$/.test(normalized))
    return normalized.split("|").includes(canonical);
  try {
    if (new RegExp(normalized).test(canonical)) return true;
  } catch {
    // An invalid pattern still matches a name it spells out.
  }
  return parts.some(
    (part) =>
      name(part).toLowerCase() === canonical.toLowerCase() ||
      claudeAliases[part] === canonical,
  );
}

/** Whether a hook whose event is compared with `target` can never run with
 * `matcher`: the event sends only certain values and it matches none. */
export function neverRuns(target: MatcherTarget | undefined, matcher: string) {
  const text = matcher.trim();
  if (!text || text === "*" || target?.compares !== "value") return false;
  return ![...(target.values ?? []), ...(target.aliases ?? [])].some((value) => matcherMatches(text, value));
}

/** The permission a tool asks for: file writes ask `edit`, browser steps
 * `browser`, generated media `media_generate`, any other tool its name. */
function permissionOf(tool: string) {
  if (["write", "multiedit", "apply_patch", "patch"].includes(tool)) return "edit";
  if (tool.startsWith("browser_")) return "browser";
  if (tool === "image_generate" || tool === "video_generate") return "media_generate";
  return tool;
}

/** Permission names to suggest: the common ones first, then whatever the
 * tools Haarness offers ask for. */
export function permissionNames(tools: string[]) {
  const common = ["bash", "read", "edit", "external_directory", "webfetch", "list", "glob", "grep"];
  return [...new Set([...common, ...tools.map(permissionOf)])];
}

/** The alternative of `value` being typed (after its last `|`), and the
 * value with a suggestion in its place. `*` replaces the whole matcher. */
export function lastAlternative(value: string) {
  const at = value.lastIndexOf("|");
  const head = at < 0 ? "" : value.slice(0, at + 1);
  return {
    query: value.slice(at + 1).trim(),
    replace: (option: string) => (option === "*" ? option : head + option),
  };
}
