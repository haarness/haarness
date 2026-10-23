/*
 * Pure presentation rules for the transcript.
 *
 * Everything here answers one question — "what should the reader see for this
 * item?" — without touching the DOM or the runtime, so the rules can be read,
 * reasoned about and tested on their own.
 */
import type { IconName } from "./icons";
import type { FileDiff, Item } from "@haarness/sdk";
import { lines, type DiffLine } from "./diff";
import { language, t } from "./i18n";

/** Provider call IDs can repeat in another turn; output belongs to one call. */
export function toolResult(items: Item[], call?: Item, boundary = Infinity) {
  if (call?.data.type !== "tool_call") return;
  const id = call.data.call_id;
  return items.find(
    (item) =>
      item.threadId === call.threadId &&
      item.turnId === call.turnId &&
      item.sequence > call.sequence &&
      item.sequence <= boundary &&
      item.data.type === "tool_result" &&
      item.data.call_id === id,
  );
}

/**
 * Tool calls are the densest thing in a transcript, so each one is reduced to a
 * verb a reader can scan ("Ran", "Read") plus the single argument that says
 * *what* it acted on. Expanded steps show readable input and output.
 */
export interface ToolShape {
  icon: IconName;
  /** What it did: "Wrote". */
  verb: string;
  /** While it runs or waits for approval: "Writing". */
  doing: string;
  /** When it failed or was refused: "Tried to write". */
  attempt: string;
  target: string;
}
type Verbs = { verb: string; doing: string; attempt: string };
/** The three forms are English catalog keys, translated where the row
 * renders (`toolVerb`). Each is a whole phrase: "Tried to " + a verb would
 * not translate. */
const verbs = (verb: string, doing: string, attempt: string): Verbs => ({
  verb,
  doing,
  attempt,
});
/**
 * Haarness's own tools are named, so they get the exact verb a reader expects.
 * Anything else — an MCP server, an extension — falls back to a family guess,
 * and if even that fails the tool's own name is the honest label.
 */
const namedTools: Record<string, { icon: IconName } & Verbs> = {
  bash: { icon: "terminal", ...verbs("Ran", "Running", "Tried to run") },
  read: { icon: "file-text", ...verbs("Read", "Reading", "Tried to read") },
  write: { icon: "edit", ...verbs("Wrote", "Writing", "Tried to write") },
  edit: { icon: "edit", ...verbs("Edited", "Editing", "Tried to edit") },
  multiedit: { icon: "edit", ...verbs("Edited", "Editing", "Tried to edit") },
  apply_patch: { icon: "edit", ...verbs("Edited", "Editing", "Tried to edit") },
  list: { icon: "folder", ...verbs("Listed", "Listing", "Tried to list") },
  glob: { icon: "search", ...verbs("Matched", "Matching", "Tried to match") },
  grep: { icon: "search", ...verbs("Searched", "Searching", "Tried to search") },
  webfetch: { icon: "web", ...verbs("Fetched", "Fetching", "Tried to fetch") },
  task: { icon: "agents", ...verbs("Delegated", "Delegating", "Tried to delegate") },
  skill: { icon: "book", ...verbs("Used", "Using", "Tried to use") },
  question: { icon: "question", ...verbs("Asked", "Asking", "Tried to ask") },
  todo: { icon: "todo", ...verbs("Updated the plan", "Updating the plan", "Tried to update the plan") },
  todowrite: { icon: "todo", ...verbs("Updated the plan", "Updating the plan", "Tried to update the plan") },
  todoread: { icon: "todo", ...verbs("Checked the plan", "Checking the plan", "Tried to check the plan") },
  batch: { icon: "layers", ...verbs("Ran a batch", "Running a batch", "Tried to run a batch") },
  // The rest of Haarness's own tools. Their names would otherwise be shown
  // as they are (`mcp_manage`) or read as the wrong family (`agent_manage`
  // as a delegation, `command_manage` as a shell command).
  websearch: { icon: "web", ...verbs("Searched the web", "Searching the web", "Tried to search the web") },
  web_search: { icon: "web", ...verbs("Searched the web", "Searching the web", "Tried to search the web") },
  execute: { icon: "code", ...verbs("Ran a script", "Running a script", "Tried to run a script") },
  lsp: { icon: "code", ...verbs("Looked up code", "Looking up code", "Tried to look up code") },
  preview: { icon: "eye", ...verbs("Showed a preview", "Showing a preview", "Tried to show a preview") },
  image_generate: { icon: "image", ...verbs("Generated an image", "Generating an image", "Tried to generate an image") },
  video_generate: { icon: "image", ...verbs("Generated a video", "Generating a video", "Tried to generate a video") },
  get_goal: { icon: "todo", ...verbs("Checked the goal", "Checking the goal", "Tried to check the goal") },
  update_goal: { icon: "todo", ...verbs("Updated the goal", "Updating the goal", "Tried to update the goal") },
  propose_goal_criteria: { icon: "todo", ...verbs("Proposed goal criteria", "Proposing goal criteria", "Tried to propose goal criteria") },
  load_tool: { icon: "tools", ...verbs("Loaded tools", "Loading tools", "Tried to load tools") },
  schedule: { icon: "clock", ...verbs("Managed schedules", "Managing schedules", "Tried to manage schedules") },
  workflow: { icon: "layers", ...verbs("Managed workflows", "Managing workflows", "Tried to manage workflows") },
  marketplace_manage: { icon: "plugin", ...verbs("Used the marketplace", "Using the marketplace", "Tried to use the marketplace") },
  capability_manage: { icon: "plugin", ...verbs("Managed capabilities", "Managing capabilities", "Tried to manage capabilities") },
  skill_manage: { icon: "book", ...verbs("Managed skills", "Managing skills", "Tried to manage skills") },
  plugin_manage: { icon: "plugin", ...verbs("Managed plugins", "Managing plugins", "Tried to manage plugins") },
  mcp_manage: { icon: "server", ...verbs("Managed MCP servers", "Managing MCP servers", "Tried to manage MCP servers") },
  mcp_resource_list: { icon: "server", ...verbs("Listed MCP resources", "Listing MCP resources", "Tried to list MCP resources") },
  mcp_resource_read: { icon: "file-text", ...verbs("Read an MCP resource", "Reading an MCP resource", "Tried to read an MCP resource") },
  agent_manage: { icon: "agents", ...verbs("Managed agents", "Managing agents", "Tried to manage agents") },
  command_manage: { icon: "compose", ...verbs("Managed commands", "Managing commands", "Tried to manage commands") },
  cloud_agent_manage: { icon: "cloud", ...verbs("Used Haarness Cloud", "Using Haarness Cloud", "Tried to use Haarness Cloud") },
  cloud_agent_session: { icon: "cloud", ...verbs("Used Haarness Cloud", "Using Haarness Cloud", "Tried to use Haarness Cloud") },
  cloud_chat: { icon: "cloud", ...verbs("Used Haarness Cloud", "Using Haarness Cloud", "Tried to use Haarness Cloud") },
  cloud_schedule: { icon: "cloud", ...verbs("Used Haarness Cloud", "Using Haarness Cloud", "Tried to use Haarness Cloud") },
  memory_list: { icon: "book", ...verbs("Looked through memories", "Looking through memories", "Tried to look through memories") },
  memory_search: { icon: "book", ...verbs("Looked through memories", "Looking through memories", "Tried to look through memories") },
  memory_read: { icon: "book", ...verbs("Read a memory", "Reading a memory", "Tried to read a memory") },
  memory_save: { icon: "book", ...verbs("Saved a memory", "Saving a memory", "Tried to save a memory") },
  memory_delete: { icon: "trash", ...verbs("Deleted a memory", "Deleting a memory", "Tried to delete a memory") },
  task_abort: { icon: "stop", ...verbs("Stopped a task", "Stopping a task", "Tried to stop a task") },
  agent_switch: { icon: "agents", ...verbs("Switched agents", "Switching agents", "Tried to switch agents") },
  session_manage: { icon: "chat", ...verbs("Managed chats", "Managing chats", "Tried to manage chats") },
  session_rename: { icon: "chat", ...verbs("Renamed a chat", "Renaming a chat", "Tried to rename a chat") },
  session_message: { icon: "chat", ...verbs("Wrote to a chat", "Writing to a chat", "Tried to write to a chat") },
  session_wait: { icon: "chat", ...verbs("Waited for a chat", "Waiting for a chat", "Tried to wait for a chat") },
  structuredoutput: { icon: "check", ...verbs("Returned a result", "Returning a result", "Tried to return a result") },
};
/** The tools the family guesses below were written for, whose running
    phrase needs no target ("Running a command", "Reading files"). Every
    other named tool's own "doing" form says it all ("Searching the web"). */
const familyTools = new Set([
  "bash", "read", "write", "edit", "multiedit", "apply_patch", "list", "glob", "grep", "webfetch", "task", "batch",
]);
/** Values a row never shows: ids that mean nothing to a reader (a UUID, a
    ULID, a long run of hex), whatever argument carries them. */
const opaqueId = (value: string) =>
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value) ||
  /^[0-9A-HJKMNP-TV-Z]{26}$/.test(value) ||
  /^[0-9a-f]{16,}$/i.test(value);
/** The arguments a row of Haarness's other tools may name, in the order a
    reader recognises them: what it is called before where it is. Its
    `action` (`add`, `rename`) is left out: the verb already says what was
    done, and an English word in another language's row would only jar. */
const humanFields = ["title", "name", "label", "agent", "query", "path", "filePath", "file_path", "url", "prompt", "description"];
function humanTarget(record: Record<string, unknown> | undefined) {
  for (const key of humanFields) {
    const value = record?.[key];
    if (typeof value !== "string") continue;
    const text = value.replace(/\s+/g, " ").trim();
    if (text && !opaqueId(text)) return text;
  }
  return "";
}
const toolKind = (name: string) =>
  /bash|shell|exec|command/.test(name)
    ? "run"
    : /write|edit|patch|apply/.test(name)
      ? "edit"
      : /read|open|cat|view/.test(name)
        ? "read"
        : /search|grep|find|glob|list/.test(name)
          ? "search"
          : /fetch|web|http|browse/.test(name)
            ? "web"
            : /task|agent|spawn/.test(name)
              ? "agent"
              : "other";
const kindShape: Record<string, { icon: IconName } & Verbs> = {
  run: { icon: "terminal", ...verbs("Ran", "Running", "Tried to run") },
  edit: { icon: "edit", ...verbs("Edited", "Editing", "Tried to edit") },
  read: { icon: "file-text", ...verbs("Read", "Reading", "Tried to read") },
  search: { icon: "search", ...verbs("Searched", "Searching", "Tried to search") },
  web: { icon: "web", ...verbs("Fetched", "Fetching", "Tried to fetch") },
  agent: { icon: "agents", ...verbs("Delegated", "Delegating", "Tried to delegate") },
  other: { icon: "tools", ...verbs("Used", "Using", "Tried to use") },
};
export function describeTool(name: string, args: unknown): ToolShape {
  name = toolName(name);
  if (name.startsWith("browser_")) return describeBrowser(name.slice(8), object(args));
  const known = namedTools[name];
  const record = object(args);
  if (
    (name === "grep" || name === "glob") &&
    typeof record?.pattern === "string"
  )
    return {
      ...known,
      target:
        typeof record.path === "string"
          ? t("{pattern} in {path}", { pattern: record.pattern, path: record.path })
          : record.pattern,
    };
  if (name === "list" && ["", ".", "./"].includes(toolTarget(args)))
    // The root has no name of its own; "." would only make a reader pause.
    return { ...known, target: t("the project folder") };
  if (name === "apply_patch" && typeof record?.input === "string") {
    // The envelope is instructions for the tool; the reader wants the files.
    const files = patchFiles(record.input).map((file) => file.path);
    if (files.length)
      return {
        ...known,
        target:
          targetList(files),
      };
  }
  // A resource row names the resource it read, or the server it listed.
  if (name === "mcp_resource_read" || name === "mcp_resource_list") {
    const value = name === "mcp_resource_read" ? record?.uri : record?.server;
    return { ...known, target: typeof value === "string" ? value.trim() : "" };
  }
  if (name === "load_tool" && Array.isArray(record?.queries))
    return { ...known, target: targetList(record.queries.filter((query): query is string => typeof query === "string")) };
  if (known && !familyTools.has(name) && !["execute", "skill", "question", "todo", "todowrite", "todoread"].includes(name))
    return { ...known, target: humanTarget(record) };
  if (known) return { ...known, target: toolTarget(args) };
  const kind = toolKind(name);
  // An unrecognised tool is named rather than described; guessing a verb for
  // it would claim knowledge the interface does not have.
  const named = kind === "other" ? { verb: name, doing: name, attempt: name } : undefined;
  return { ...kindShape[kind], ...named, target: toolTarget(args) };
}
/**
 * Browser steps read like what a person did in the page: "Opened example.com",
 * "Typed “jane@example.com”", "Clicked". Element refs (@e4) mean nothing to a
 * reader, so the target is the URL, text, value or key when there is one.
 */
const browserVerbs: Record<string, Verbs> = {
  navigate: verbs("Opened", "Opening", "Tried to open"),
  tabs_open: verbs("Opened a tab", "Opening a tab", "Tried to open a tab"),
  tabs_close: verbs("Closed a tab", "Closing a tab", "Tried to close a tab"),
  tabs_focus: verbs("Switched tabs", "Switching tabs", "Tried to switch tabs"),
  tabs_list: verbs("Listed tabs", "Listing tabs", "Tried to list tabs"),
  snapshot: verbs("Read the page", "Reading the page", "Tried to read the page"),
  find: verbs("Searched the page", "Searching the page", "Tried to search the page"),
  click: verbs("Clicked", "Clicking", "Tried to click"),
  hover: verbs("Pointed at an element", "Pointing at an element", "Tried to point at an element"),
  drag: verbs("Dragged", "Dragging", "Tried to drag"),
  fill: verbs("Typed", "Typing", "Tried to type"),
  fill_form: verbs("Filled in a form", "Filling in a form", "Tried to fill in a form"),
  select: verbs("Selected", "Selecting", "Tried to select"),
  press: verbs("Pressed", "Pressing", "Tried to press"),
  scroll: verbs("Scrolled", "Scrolling", "Tried to scroll"),
  wait: verbs("Waited", "Waiting", "Tried to wait"),
  screenshot: verbs("Took a screenshot", "Taking a screenshot", "Tried to take a screenshot"),
  evaluate: verbs("Ran a page script", "Running a page script", "Tried to run a page script"),
  back: verbs("Went back", "Going back", "Tried to go back"),
  forward: verbs("Went forward", "Going forward", "Tried to go forward"),
  reload: verbs("Reloaded the page", "Reloading the page", "Tried to reload the page"),
  stop: verbs("Stopped loading", "Stopping loading", "Tried to stop loading"),
  dialog: verbs("Handled a dialog", "Handling a dialog", "Tried to handle a dialog"),
  console: verbs("Read the console", "Reading the console", "Tried to read the console"),
  network_list: verbs("Listed network requests", "Listing network requests", "Tried to list network requests"),
  network_get: verbs("Read a network request", "Reading a network request", "Tried to read a network request"),
};
const checkVerbs = verbs("Checked", "Checking", "Tried to check");
const uncheckVerbs = verbs("Unchecked", "Unchecking", "Tried to uncheck");
/** Catalog keys a tool row can show, for the catalog guard. */
export const toolVerbKeys = [
  ...new Set(
    [
      ...Object.values(namedTools),
      ...Object.values(kindShape),
      ...Object.values(browserVerbs),
      checkVerbs,
      uncheckVerbs,
    ].flatMap(({ verb, doing, attempt }) => [verb, doing, attempt]),
  ),
];
/** These verbs are already catalog keys in another sense ("Read" the
 * permission, "Running" the status), so a tool row has keys of its own. */
const toolVerbKeyOverrides: Record<string, string> = {
  Read: "Read (tool step)",
  Running: "Running (tool step)",
  Selected: "Selected (tool step)",
};
export const toolVerbKey = (verb: string) => toolVerbKeyOverrides[verb] ?? verb;
/** A tool row's verb in the interface language. Unknown tools are named by
 * the tool itself, which no catalog has, so they pass through unchanged. */
export const toolVerb = (verb: string) => t(toolVerbKey(verb));
/** Phrases after the first continue one sentence, so they start in lower
 * case (German nouns keep their capitals). English keeps the comma list the
 * activity line has always shown; other languages join with their own word
 * for "and". */
function phraseList(phrases: string[]) {
  const code = language();
  const parts = phrases.map((phrase, index) =>
    index && code !== "de"
      ? phrase.charAt(0).toLocaleLowerCase(code) + phrase.slice(1)
      : phrase,
  );
  return code === "en"
    ? parts.join(", ")
    : new Intl.ListFormat(code, { type: "conjunction" }).format(parts);
}
function describeBrowser(action: string, record?: Record<string, unknown>): ToolShape {
  const text = (value: unknown) =>
    typeof value === "string" && value.trim() ? value.replace(/\s+/g, " ").trim() : "";
  const quoted = (value: unknown) => (text(value) ? `“${text(value)}”` : "");
  const target =
    action === "fill"
      ? quoted(record?.text)
      : action === "select" && Array.isArray(record?.values)
        ? record.values.map(quoted).filter(Boolean).join(", ")
        : action === "press"
          ? text(record?.key)
          : action === "find" || action === "wait"
            ? quoted(record?.text ?? record?.query)
            : action === "scroll" && typeof record?.deltaY === "number"
              ? record.deltaY < 0 ? t("up") : t("down")
              : text(record?.url);
  const fallback = "Browser · " + action.replaceAll("_", " ");
  const forms =
    action === "check"
      ? record?.checked === false
        ? uncheckVerbs
        : checkVerbs
      : (browserVerbs[action] ?? { verb: fallback, doing: fallback, attempt: fallback });
  return { icon: "web", ...forms, target };
}
/** Normalize only the runtime's known wrapper and built-in names. */
export function toolName(name: string) {
  const value = name.replace(/^(?:functions\.)+/i, "");
  return namedTools[value.toLowerCase()] ? value.toLowerCase() : value;
}
const object = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
const label = (key: string) => {
  const value = key
    .replace(/([a-z])([A-Z])/g, "$1 $2")
    .replace(/[_-]+/g, " ");
  return value.charAt(0).toUpperCase() + value.slice(1);
};
/** Render structured tool data as text, never as a serialized argument envelope. */
export function readable(value: unknown, depth = 0): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  if (typeof value === "boolean") return value ? "Yes" : "No";
  if (typeof value !== "object") return String(value);
  if (depth >= 8) return "…";
  if (Array.isArray(value))
    return value
      .map((item) => readable(item, depth + 1))
      .filter(Boolean)
      .map((text) => "• " + text.replace(/\n/g, "\n  "))
      .join("\n");
  return Object.entries(value)
    .flatMap(([key, item]) => {
      const text = readable(item, depth + 1);
      return text ? [`${label(key)}: ${text}`] : [];
    })
    .join("\n");
}
export function toolInput(name: string, args: unknown) {
  name = toolName(name);
  const record = object(args);
  const command = record?.command ?? record?.cmd;
  if (toolKind(name) === "run" && typeof command === "string") {
    const directory = record?.workdir ?? record?.cwd;
    return {
      label: "Shell",
      text: "$ " + command.trim(),
      context: typeof directory === "string" ? directory : "",
    };
  }
  return {
    label:
      (
        {
          read: "File",
          list: "Files",
          glob: "Search",
          grep: "Search",
          webfetch: "Web page",
          execute: "Code",
        } as Record<string, string>
      )[name] ?? "Tool",
    text: readable(args),
    context: "",
  };
}
export function toolOutput(name: string, output: string) {
  // Actual source files and command output must stay verbatim, including JSON
  // the user deliberately read or printed. Only tool envelopes are formatted.
  if (
    ["read", "bash", "grep", "glob", "list", "webfetch"].includes(
      toolName(name),
    )
  )
    return output;
  try {
    return readable(JSON.parse(output));
  } catch {
    return output;
  }
}
export interface ToolResult {
  output: string;
  is_error: boolean;
  metadata?: unknown;
}
/**
 * The user's own Chrome, when a browser step ran there. The server reports
 * the browser in the result; until a result arrives, or when the step failed
 * before reaching a browser, a tab opened with `browser: "chrome"` says where
 * it was going. A success without the report came from a server that has
 * only the Haarness browser.
 */
export function ranInChrome(name: string, args: unknown, result?: ToolResult) {
  if (!toolName(name).startsWith("browser_")) return false;
  const reported = object(result?.metadata)?.browser;
  if (reported === "chrome" || reported === "haarness")
    return reported === "chrome";
  if (result && !result.is_error) return false;
  return object(args)?.browser === "chrome";
}
/** A question a `PreToolUse` hook answered, so nobody was asked. */
export function answeredByHook(name: string, result?: ToolResult) {
  return toolName(name) === "question" && object(result?.metadata)?.answeredBy === "hook";
}
export function toolFailed(name: string, result?: ToolResult) {
  if (!result) return false;
  if (result.is_error) return true;
  const exit = object(result.metadata)?.exit;
  return toolName(name) === "bash" && typeof exit === "number" && exit !== 0;
}
export interface BatchStep {
  name: string;
  args: unknown;
  result?: ToolResult;
}
export function batchSteps(args: unknown, result?: ToolResult): BatchStep[] {
  const calls = object(args)?.tool_calls;
  if (
    !Array.isArray(calls) ||
    !calls.length ||
    calls.length > 25 ||
    !calls.every((call) => typeof object(call)?.tool === "string")
  )
    return [];
  let payload = object(result?.metadata);
  if (!Array.isArray(payload?.results) && result) {
    try {
      payload = object(JSON.parse(result.output));
    } catch {
      /* Plain errors are shown on the batch. */
    }
  }
  const results = Array.isArray(payload?.results) ? payload.results : [];
  return calls.map((call, index) => {
    const entry = object(results[index]);
    // The server returns input order. Never match by tool name alone: a batch
    // can read the same file more than once with different ranges or fail early.
    const matched =
      typeof entry?.tool === "string" &&
      toolName(entry.tool) === toolName(call.tool);
    const error =
      matched && typeof entry.error === "string" ? entry.error : undefined;
    const output =
      matched && typeof entry.output === "string" ? entry.output : undefined;
    return {
      name: call.tool,
      args: call.parameters,
      result:
        error !== undefined || output !== undefined
          ? {
              output: error ?? output!,
              is_error: error !== undefined,
              metadata: entry?.metadata,
            }
          : undefined,
    };
  });
}
/** What a finished batch did, as English catalog keys. */
const batchDone = {
  list: "Listed files",
  plan: "Updated the plan",
  run: "Ran commands",
  read: "Read files",
  edit: "Edited files",
  search: "Searched files",
  web: "Fetched pages",
  agent: "Delegated tasks",
  other: "Used tools",
} as const;
/** What a running batch is doing. */
const batchDoing = {
  list: "Listing files",
  plan: "Updating the plan",
  run: "Running commands",
  read: "Reading files",
  edit: "Editing files",
  search: "Searching files",
  web: "Reading web pages",
  agent: "Working with agents",
  other: "Using tools",
} as const;
/** What a single running tool is doing; any other tool is named. */
const toolDoing = {
  run: "Running a command",
  read: "Reading files",
  edit: "Editing files",
  search: "Searching files",
  web: "Reading a web page",
  agent: "Working with an agent",
} as const;
/** Named tools whose "doing" form needs its target ("Using", "Asking"), as the activity line words them alone. */
const ownActivity: Record<string, string> = {
  skill: "Using a skill",
  question: "Asking a question",
};
/** The activity keys reached through the tables above, for the catalog guard. */
export const activityKeys = [
  ...new Set([
    ...Object.values(batchDone),
    ...Object.values(batchDoing),
    ...Object.values(toolDoing),
    ...Object.values(ownActivity),
  ]),
];
const batchGroup = (tool: string) => {
  const name = toolName(tool);
  if (name === "list") return "list";
  if (["todo", "todowrite", "todoread"].includes(name)) return "plan";
  // A tool of Haarness's own outside the families is not guessed into one.
  if (namedTools[name] && !familyTools.has(name)) return "other";
  return toolKind(name);
};
export function batchLabel(steps: BatchStep[]) {
  const actions = [...new Set(steps.map((step) => batchDone[batchGroup(step.name)]))];
  return phraseList(actions.map((action) => t(action)));
}
/**
 * What a batch acted on, for its collapsed row: the steps' distinct targets,
 * the first two and then "+N", as an apply_patch row lists its files. Rows
 * start collapsed, and "Read files" alone does not say which.
 */
export function batchTarget(steps: BatchStep[]) {
  return targetList([
    ...new Set(steps.map((step) => describeTool(step.name, step.args).target).filter(Boolean)),
  ]);
}
/**
 * A row's targets as the interface language lists them: one, two, or the
 * first two and how many more ("a.ts, b.ts +3").
 */
export function targetList(targets: string[]) {
  if (targets.length < 2) return targets[0] ?? "";
  const [first, second] = targets;
  return targets.length === 2
    ? t("{first}, {second}", { first, second })
    : t("{first}, {second} +{count}", { first, second, count: String(targets.length - 2) });
}
/** The argument a tool row names, as the call wrote it. */
function targetText(args: unknown): string {
  if (!args || typeof args !== "object") return "";
  const record = args as Record<string, unknown>;
  for (const key of [
    "command",
    "path",
    "file_path",
    "filePath",
    "pattern",
    "query",
    "url",
    "prompt",
    "description",
    "name",
  ]) {
    const value = record[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  const first = Object.values(record).find(
    (value) => typeof value === "string" && value.trim(),
  );
  return typeof first === "string" ? first.trim() : "";
}
export function toolTarget(args: unknown): string {
  return targetText(args).replace(/\s+/g, " ");
}
/**
 * A tool row's tooltip: the whole target with its line breaks, so a
 * multi-line command reads as written, not as the one flattened line the row
 * has room for. A target the row words itself ("the project folder") is
 * shown as the row shows it.
 */
export function toolTitle(args: unknown, target: string): string {
  const text = targetText(args);
  return text.replace(/\s+/g, " ") === target ? text : target;
}
// A permission detail is usually a shell command; show that line plainly and
// keep the raw payload only when there is nothing better to say.
export function permissionCommand(detail: unknown) {
  if (!detail || typeof detail !== "object") return "";
  const record = detail as Record<string, unknown>;
  const value =
    record.command ?? record.path ?? record.file_path ?? record.url;
  return typeof value === "string" ? value : "";
}

/**
 * The script a shell request's command runs, as the server read it when it
 * asked: its path (inside the project, relative to it), its lines without
 * the final line break, and whether the file goes on. Nothing for any other
 * request, or an older server that sends none.
 */
export function permissionScript(
  permission: { permission?: string; script?: unknown },
  workspace = "",
) {
  const script = permission.permission === "bash" ? permission.script : undefined;
  if (!script || typeof script !== "object") return undefined;
  const { path, content, truncated } = script as Record<string, unknown>;
  if (typeof path !== "string" || typeof content !== "string") return undefined;
  const text = content.replace(/\n$/, "");
  const root = workspace.replace(/\/+$/, "");
  return {
    path: root && path.startsWith(root + "/") ? path.slice(root.length + 1) : path,
    lines: text ? text.split("\n").map(expandTabs) : [],
    truncated: truncated === true,
  };
}

/** A line with its tabs as spaces to the next stop of four, as the terminal
 * UI draws them, so a wrapped line's hanging indent cannot shift them. */
function expandTabs(line: string) {
  if (!line.includes("\t")) return line;
  let text = "";
  for (const character of line)
    text += character === "\t" ? " ".repeat(4 - (text.length % 4)) : character;
  return text;
}

/**
 * An edit is a change, so it should be shown as one. When a tool call carries
 * an edit shape — a replacement, a batch of replacements, a whole new file, or
 * a patch — the disclosure shows the diff instead of the raw arguments.
 */
export interface Change {
  path: string;
  before?: string;
  after?: string;
  patch?: string;
  additions?: number;
  deletions?: number;
}
/**
 * The files in a Codex-style patch envelope (`*** Begin Patch` …
 * `*** End Patch`), each as a unified diff the renderer already understands.
 * Line numbers are unknown until the tool runs, so hunks start at 1; previews
 * hide hunk headers, and the result's own diffs replace these once it lands.
 */
export function patchFiles(text: string): (Change & { path: string })[] {
  if (!/^\*\*\* Begin Patch/m.test(text)) return [];
  const files: { path: string; hunks: string[][] }[] = [];
  let file: (typeof files)[number] | undefined;
  let hunk: string[] | undefined;
  for (const line of text.split("\n")) {
    const header = /^\*\*\* (Add|Update|Delete) File: (.+)$/.exec(line);
    if (header) {
      file = { path: header[2].trim(), hunks: [] };
      hunk = undefined;
      files.push(file);
      continue;
    }
    if (!file || line.startsWith("*** ")) continue;
    if (line.startsWith("@@")) {
      hunk = [];
      file.hunks.push(hunk);
      continue;
    }
    if (!/^[ +-]/.test(line)) continue;
    if (!hunk) file.hunks.push((hunk = []));
    hunk.push(line);
  }
  return files.map(({ path, hunks }) => {
    const body = hunks
      .filter((rows) => rows.length)
      .map((rows) => {
        const before = rows.filter((row) => row[0] !== "+").length;
        const after = rows.filter((row) => row[0] !== "-").length;
        return `@@ -1,${before} +1,${after} @@\n${rows.join("\n")}`;
      });
    return {
      path,
      patch: `--- a/${path}\n+++ b/${path}\n${body.join("\n")}\n`,
      additions: hunks.flat().filter((row) => row[0] === "+").length,
      deletions: hunks.flat().filter((row) => row[0] === "-").length,
    };
  });
}
/** What an edit tool reports it changed, which beats re-deriving it from the call. */
function resultChanges(metadata: unknown): Change[] {
  const diffs = object(metadata)?.diffs;
  if (!Array.isArray(diffs)) return [];
  return diffs.flatMap((entry) => {
    const diff = object(entry);
    if (typeof diff?.file !== "string") return [];
    const count = (value: unknown) =>
      typeof value === "number" ? value : undefined;
    const text = (value: unknown) =>
      typeof value === "string" ? value : undefined;
    return [
      {
        path: diff.file,
        patch: text(diff.patch),
        before: text(diff.before) ?? (text(diff.patch) ? undefined : ""),
        after: text(diff.after) ?? (text(diff.patch) ? undefined : ""),
        additions: count(diff.additions),
        deletions: count(diff.deletions),
      },
    ];
  });
}
export function toolChanges(
  name: string,
  args: unknown,
  result?: ToolResult,
): Change[] {
  if (
    !args ||
    typeof args !== "object" ||
    !/write|edit|patch|apply/.test(name)
  )
    return [];
  // A targeted edit's own old/new snippet is the clearest preview. A patch
  // only says what to do, so the diff its tool reports is the better one.
  const reported = result && !result.is_error ? resultChanges(result.metadata) : [];
  if (reported.length && /patch/.test(name)) return reported;
  const requested = argumentChanges(args as Record<string, unknown>);
  return requested.length ? requested : reported;
}
function argumentChanges(record: Record<string, unknown>): Change[] {
  const text = (...keys: string[]) => {
    for (const key of keys) {
      const value = record[key];
      if (typeof value === "string") return value;
    }
    return undefined;
  };
  const path = text("path", "file_path", "filePath") ?? "";
  const patch = text("input", "patch", "diff");
  const envelope = patch ? patchFiles(patch) : [];
  if (envelope.length) return envelope;
  if (patch && /^(\*\*\*|---|@@|diff )/m.test(patch))
    return [{ path: path || "patch", patch }];
  const before = text("old", "old_string", "oldText");
  const after = text("new", "new_string", "newText");
  if (before !== undefined && after !== undefined)
    return [{ path, before, after }];
  const content = text("content");
  if (content !== undefined) return [{ path, before: "", after: content }];
  const edits = record.edits;
  if (Array.isArray(edits))
    return edits
      .filter(
        (edit): edit is Record<string, unknown> =>
          !!edit && typeof edit === "object",
      )
      .map((edit) => ({
        path,
        before: typeof edit.old === "string" ? edit.old : "",
        after: typeof edit.new === "string" ? edit.new : "",
      }));
  return [];
}
/**
 * "updated src/math.ts" under a diff of src/math.ts says nothing new. An edit's
 * output is kept only when it carries more than a routine confirmation, such as
 * diagnostics or a partial failure.
 */
export function routineEditOutput(output: string) {
  const rows = output.split("\n").map((row) => row.trim()).filter(Boolean);
  return (
    rows.length > 0 &&
    rows.length <= 20 &&
    rows.every((row) =>
      /^(?:success\.?|done\.?|ok\.?)?\s*(?:updated the following files:?|[AMD] \S.*|(?:updated|added|created|deleted|modified|wrote|edited|moved)\b[^:]*)?$/i.test(
        row,
      ),
    )
  );
}
/** Transcript diffs stay a preview; the Review panel is where the whole file lives. */
export const CHANGE_PREVIEW_LINES = 24;

/**
 * The lines a change preview shows. An edit's snippets rarely end in a newline,
 * and a line diff reads "a" and "a\n" as different lines, so the unchanged last
 * line of `old` would show as removed and added again. Both sides end in one
 * before they are compared.
 */
export function changeRows(change: Change): DiffLine[] {
  const ended = (text = "") => (text && !text.endsWith("\n") ? text + "\n" : text);
  return lines({
    file: change.path || "change",
    patch: change.patch,
    before: ended(change.before),
    after: ended(change.after),
  } as FileDiff).filter((row) => row.kind !== "hunk");
}

/** Lines added and removed across changes, counted from the rows a reader sees. */
export function changeStats(changes: Change[]) {
  let additions = 0,
    deletions = 0;
  for (const change of changes) {
    if (change.additions !== undefined && change.deletions !== undefined) {
      additions += change.additions;
      deletions += change.deletions;
      continue;
    }
    for (const row of changeRows(change))
      if (row.kind === "add") additions++;
      else if (row.kind === "remove") deletions++;
  }
  return additions || deletions ? { additions, deletions } : undefined;
}

/** The tail of a path, which is the part that identifies a checkout to a reader. */
export function shortPath(value: string, keep = 2) {
  const parts = value.split("/").filter(Boolean);
  return parts.length > keep
    ? "…/" + parts.slice(-keep).join("/")
    : "/" + parts.join("/");
}

/**
 * Errors are shown where they happened, with the one action that actually
 * helps. A missing credential is by far the most common failure, and its fix
 * lives in Settings rather than in a retry.
 */
export function errorRecovery(message: string) {
  return /api key|credential|unauthor|not configured|authenticat/i.test(
    message,
  )
    ? ({ label: "Open providers", pane: "providers" } as const)
    : null;
}

/** The tools inside a running batch: from its arguments when the caller has
 * them, or from the server's `batch a,b` summary for a delegated task's
 * progress, which carries no arguments. */
function batchTools(name: string, args?: unknown): string[] {
  const value = toolName(name);
  if (value === "batch") return batchSteps(args).map((step) => step.name);
  const listed = /^batch\s+(.+)$/.exec(value)?.[1];
  return listed
    ? listed
        .split(",")
        .map((entry) => entry.trim())
        .filter(Boolean)
    : [];
}
/** Shared live activity labels for the main reply and delegated task rows. A
 * batch is described by what its steps do, never by the wrapper's name: three
 * delegations read as agents at work, a sweep of reads as reading. */
export function toolActivity(name: string, args?: unknown): string {
  const inner = batchTools(name, args);
  if (inner.length) {
    const phrases = [...new Set(inner.map((tool) => batchDoing[batchGroup(tool)]))];
    return phraseList(phrases.map((phrase) => t(phrase)));
  }
  if (toolName(name) === "batch") return t("Working through several steps");
  const known = namedTools[toolName(name)];
  if (Object.hasOwn(ownActivity, toolName(name))) return t(ownActivity[toolName(name)]);
  if (known && !familyTools.has(toolName(name))) return t(known.doing);
  const kind = toolKind(name);
  return kind === "other"
    ? t("Using {tool}", { tool: name })
    : t(toolDoing[kind]);
}
