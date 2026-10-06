/*
 * Pure presentation rules for the transcript.
 *
 * Everything here answers one question — "what should the reader see for this
 * item?" — without touching the DOM or the runtime, so the rules can be read,
 * reasoned about and tested on their own.
 */
import type { IconName } from "./icons";
import type { Item } from "@haarness/sdk";

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
const verbs = (verb: string, doing: string, base: string): Verbs => ({
  verb,
  doing,
  attempt: "Tried to " + base,
});
/**
 * Haarness's own tools are named, so they get the exact verb a reader expects.
 * Anything else — an MCP server, an extension — falls back to a family guess,
 * and if even that fails the tool's own name is the honest label.
 */
const namedTools: Record<string, { icon: IconName } & Verbs> = {
  bash: { icon: "terminal", ...verbs("Ran", "Running", "run") },
  read: { icon: "file-text", ...verbs("Read", "Reading", "read") },
  write: { icon: "edit", ...verbs("Wrote", "Writing", "write") },
  edit: { icon: "edit", ...verbs("Edited", "Editing", "edit") },
  multiedit: { icon: "edit", ...verbs("Edited", "Editing", "edit") },
  apply_patch: { icon: "edit", ...verbs("Edited", "Editing", "edit") },
  list: { icon: "folder", ...verbs("Listed", "Listing", "list") },
  glob: { icon: "search", ...verbs("Matched", "Matching", "match") },
  grep: { icon: "search", ...verbs("Searched", "Searching", "search") },
  webfetch: { icon: "web", ...verbs("Fetched", "Fetching", "fetch") },
  task: { icon: "agents", ...verbs("Delegated", "Delegating", "delegate") },
  skill: { icon: "book", ...verbs("Used", "Using", "use") },
  question: { icon: "question", ...verbs("Asked", "Asking", "ask") },
  todo: { icon: "todo", ...verbs("Updated the plan", "Updating the plan", "update the plan") },
  todowrite: { icon: "todo", ...verbs("Updated the plan", "Updating the plan", "update the plan") },
  todoread: { icon: "todo", ...verbs("Checked the plan", "Checking the plan", "check the plan") },
  batch: { icon: "layers", ...verbs("Ran a batch", "Running a batch", "run a batch") },
};
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
  run: { icon: "terminal", ...verbs("Ran", "Running", "run") },
  edit: { icon: "edit", ...verbs("Edited", "Editing", "edit") },
  read: { icon: "file-text", ...verbs("Read", "Reading", "read") },
  search: { icon: "search", ...verbs("Searched", "Searching", "search") },
  web: { icon: "web", ...verbs("Fetched", "Fetching", "fetch") },
  agent: { icon: "agents", ...verbs("Delegated", "Delegating", "delegate") },
  other: { icon: "tools", ...verbs("Used", "Using", "use") },
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
        record.pattern +
        (typeof record.path === "string" ? ` in ${record.path}` : ""),
    };
  if (name === "list" && ["", ".", "./"].includes(toolTarget(args)))
    // The root has no name of its own; "." would only make a reader pause.
    return { ...known, target: "the project folder" };
  if (name === "apply_patch" && typeof record?.input === "string") {
    // The envelope is instructions for the tool; the reader wants the files.
    const files = patchFiles(record.input).map((file) => file.path);
    if (files.length)
      return {
        ...known,
        target:
          files.length > 2
            ? `${files[0]}, ${files[1]} +${files.length - 2}`
            : files.join(", "),
      };
  }
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
  navigate: verbs("Opened", "Opening", "open"),
  tabs_open: verbs("Opened a tab", "Opening a tab", "open a tab"),
  tabs_close: verbs("Closed a tab", "Closing a tab", "close a tab"),
  tabs_focus: verbs("Switched tabs", "Switching tabs", "switch tabs"),
  tabs_list: verbs("Listed tabs", "Listing tabs", "list tabs"),
  snapshot: verbs("Read the page", "Reading the page", "read the page"),
  find: verbs("Searched the page", "Searching the page", "search the page"),
  click: verbs("Clicked", "Clicking", "click"),
  hover: verbs("Pointed at an element", "Pointing at an element", "point at an element"),
  drag: verbs("Dragged", "Dragging", "drag"),
  fill: verbs("Typed", "Typing", "type"),
  fill_form: verbs("Filled in a form", "Filling in a form", "fill in a form"),
  select: verbs("Selected", "Selecting", "select"),
  press: verbs("Pressed", "Pressing", "press"),
  scroll: verbs("Scrolled", "Scrolling", "scroll"),
  wait: verbs("Waited", "Waiting", "wait"),
  screenshot: verbs("Took a screenshot", "Taking a screenshot", "take a screenshot"),
  evaluate: verbs("Ran a page script", "Running a page script", "run a page script"),
  back: verbs("Went back", "Going back", "go back"),
  forward: verbs("Went forward", "Going forward", "go forward"),
  reload: verbs("Reloaded the page", "Reloading the page", "reload the page"),
  stop: verbs("Stopped loading", "Stopping loading", "stop loading"),
  dialog: verbs("Handled a dialog", "Handling a dialog", "handle a dialog"),
  console: verbs("Read the console", "Reading the console", "read the console"),
  network_list: verbs("Listed network requests", "Listing network requests", "list network requests"),
  network_get: verbs("Read a network request", "Reading a network request", "read a network request"),
};
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
              ? record.deltaY < 0 ? "up" : "down"
              : text(record?.url);
  const fallback = "Browser · " + action.replaceAll("_", " ");
  const forms =
    action === "check"
      ? record?.checked === false
        ? verbs("Unchecked", "Unchecking", "uncheck")
        : verbs("Checked", "Checking", "check")
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
export function batchLabel(steps: BatchStep[]) {
  const actions = [
    ...new Set(
      steps.map((step) => {
        const name = toolName(step.name);
        if (name === "list") return "Listed files";
        if (["todo", "todowrite", "todoread"].includes(name))
          return "Updated the plan";
        return {
          run: "Ran commands",
          read: "Read files",
          edit: "Edited files",
          search: "Searched files",
          web: "Fetched pages",
          agent: "Delegated tasks",
          other: "Used tools",
        }[toolKind(name)];
      }),
    ),
  ];
  return actions
    .map((action, index) => (index ? action.toLowerCase() : action))
    .join(", ");
}
export function toolTarget(args: unknown): string {
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
    if (typeof value === "string" && value.trim())
      return value.replace(/\s+/g, " ").trim();
  }
  const first = Object.values(record).find(
    (value) => typeof value === "string" && value.trim(),
  );
  return typeof first === "string" ? first.replace(/\s+/g, " ").trim() : "";
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
    const phrases = [
      ...new Set(
        inner.map((tool) => {
          const value = toolName(tool);
          if (value === "list") return "Listing files";
          if (["todo", "todowrite", "todoread"].includes(value))
            return "Updating the plan";
          return {
            run: "Running commands",
            read: "Reading files",
            edit: "Editing files",
            search: "Searching files",
            web: "Reading web pages",
            agent: "Working with agents",
            other: "Using tools",
          }[toolKind(value)];
        }),
      ),
    ];
    return phrases
      .map((phrase, index) => (index ? phrase.toLowerCase() : phrase))
      .join(", ");
  }
  if (toolName(name) === "batch") return "Working through several steps";
  return {
    run: "Running a command",
    read: "Reading files",
    edit: "Editing files",
    search: "Searching files",
    web: "Reading a web page",
    agent: "Working with an agent",
    other: "Using " + name,
  }[toolKind(name)];
}
