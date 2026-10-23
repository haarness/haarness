import { formatNumber, language, t } from "./i18n";
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Match,
  onCleanup,
  onMount,
  Show,
  Switch,
} from "solid-js";
import { useApp } from "./state";
import { Icon } from "./icons";
import type { FileContent } from "../shared";
import {
  activeMention,
  contextKey,
  contextLabel,
  countLines,
  fileContext,
  mentionAgents,
  pasteSize,
  RESOURCE_MENTIONS,
  sentParts,
  type PromptContext,
  type ResourceContext,
} from "./prompt-context";
import { shortPage, type ElementContext } from "./element-context";
import {
  listResources,
  resourceEntry,
  searchResources,
  type Entry,
  type ResourceOption,
} from "./resource-mentions";
import type * as T from "@haarness/sdk";
import "./prompt-context.css";

type Option =
  | { kind: "file" | "agent"; name: string; description?: string }
  | ({ kind: "resource" } & ResourceOption);
/** How a paste reads, in its chip and in the transcript: `Pasted text · 214
 * lines`, and its size once that matters, `· 2.4 MB`. */
export function pasteLabel(text: string) {
  const lines = Math.max(1, countLines(text));
  const label =
    lines === 1
      ? t("Pasted text · 1 line")
      : t("Pasted text · {count} lines", { count: formatNumber(lines) });
  const size = pasteSize(text, language());
  return size ? `${label} · ${size}` : label;
}
/** A chip's words. A resource another server also has, under the same name,
 * says which server it is read from. */
const chipLabel = (item: PromptContext, items: PromptContext[] = []) =>
  item.kind === "paste"
    ? pasteLabel(item.text)
    : item.kind === "resource" &&
        items.some((other) => other !== item && other.kind === "resource" && other.name === item.name)
      ? `${contextLabel(item)} · ${item.server}`
      : contextLabel(item);
/** Hovering a picked element's chip says what and where it is; a
 * resource's, where it is read from. */
function chipTitle(item: PromptContext) {
  if (item.kind === "resource") return `${item.uri} · ${item.server}`;
  if (item.kind !== "element") return chipLabel(item);
  return [
    `${item.tag} · ${item.width}×${item.height}`,
    shortPage(item.url),
    item.selector ?? "",
  ]
    .filter(Boolean)
    .join("\n");
}
/** Why the server left a mentioned resource unread, in the person's
 * language when Haarness wrote the reason (an MCP server's own words stay
 * as they are). */
export function unreadReason(reason: string) {
  if (reason === "a rule keeps this resource from the model") return t("a rule keeps this resource from the model");
  const late = /^did not answer within (\d+) s$/.exec(reason);
  return late ? t("did not answer within {seconds} s", { seconds: late[1] }) : reason;
}
/** A mentioned resource: what it is and where the server reads it from;
 * one brought back from a sent message, what was read then. */
function ResourcePreview(props: { item: ResourceContext }) {
  const read = () => {
    const part = props.item.block === undefined ? undefined : sentParts(props.item.block)[0];
    return part?.kind === "resource" ? part : undefined;
  };
  return (
    <>
      <small>{t("MCP resource · {server}", { server: props.item.server })}</small>
      <dl class="prompt-context-element">
        <dt>URI</dt>
        <dd dir="ltr">
          <code>{props.item.uri}</code>
        </dd>
        <Show when={props.item.description}>
          <dt>{t("Description")}</dt>
          <dd dir="auto">{props.item.description}</dd>
        </Show>
      </dl>
      <Show
        when={read()}
        fallback={
          <p class="prompt-context-note">
            {t("Read from {server} when the message is sent.", { server: props.item.server })}
          </p>
        }
      >
        {(part) => (
          <Show
            when={!part().failed}
            fallback={
              <p class="prompt-context-note">
                {t("not read")}: {unreadReason(part().content)}
              </p>
            }
          >
            <pre>{part().content}</pre>
            <p class="prompt-context-note">{t("Sent again as it was read.")}</p>
          </Show>
        )}
      </Show>
    </>
  );
}
/** A picked element as it was captured: where, what it said, its HTML. */
function ElementPreview(props: { item: ElementContext }) {
  return (
    <>
      <small>
        {t("Picked page element · {size}", {
          size: `${props.item.width}×${props.item.height}`,
        })}
      </small>
      <dl class="prompt-context-element">
        <dt>{t("Page")}</dt>
        <dd dir="ltr">{props.item.url}</dd>
        <Show when={props.item.selector}>
          <dt>{t("Selector")}</dt>
          <dd dir="ltr">
            <code>{props.item.selector}</code>
          </dd>
        </Show>
        <Show when={props.item.text}>
          <dt>{t("Text")}</dt>
          <dd dir="auto">{props.item.text}</dd>
        </Show>
      </dl>
      <pre dir="ltr">{props.item.html}</pre>
    </>
  );
}
export function PromptContextControls(props: {
  text: string;
  textarea: () => HTMLTextAreaElement;
  change: (value: string) => void;
  /** Text into the message box at its caret, as typing it would. */
  insert: (text: string) => void;
  disabled: boolean;
  /** Whether new MCP resources may be picked (not while a queued message,
   * whose edit saves text only, is edited). */
  resources?: boolean;
  /** The chat (or, before one exists, the agent) whose rules decide which
   * servers' resources are left out. */
  chat?: { threadId?: string; agent?: string };
}) {
  const app = useApp();
  const [caret, setCaret] = createSignal(0),
    [cursor, setCursor] = createSignal(0);
  const [files, setFiles] = createSignal<string[]>([]),
    [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false),
    [busy, setBusy] = createSignal(false);
  const [dismissed, setDismissed] = createSignal("");
  const mention = () => activeMention(props.text, caret());
  const signature = () =>
    JSON.stringify([props.text, caret(), app.state.connection?.id]);
  const open = () =>
    !!mention() && dismissed() !== signature() && !props.disabled;
  /** The connection's MCP resources, asked for the first time `@` opens
   * there (`resource-mentions.ts`), and the servers this chat's rules take
   * away; an older server reads none. */
  const [resources, setResources] = createSignal<{ connection: string; entry: Entry; off: Set<string> }>();
  const offersResources = () =>
    props.resources !== false && !!app.state.connection?.info.capabilities.includes(RESOURCE_MENTIONS);
  // Only opening the list (or a project or chat switch) asks; typing in it does not.
  const opened = createMemo(open);
  createEffect(() => {
    const connection = app.state.connection?.id;
    const chat = props.chat ?? {};
    if (!opened() || !connection || !offersResources()) return;
    void window.haarness
      .api<T.McpServerInfo[]>(connection, "mcpServers", [chat])
      .then((servers) => {
        // An answer for a project left meanwhile is not shown.
        if (app.state.connection?.id !== connection) return;
        setResources({
          connection,
          entry: resourceEntry(connection, (add) => listResources(connection, servers, add)),
          off: new Set(servers.filter((server) => server.turnedOff).map((server) => server.name)),
        });
      })
      .catch(() => undefined);
  });
  const shownResources = () => {
    const known = resources();
    return known && known.connection === app.state.connection?.id && offersResources() ? known : undefined;
  };
  const resourceRows = (query: string): Option[] => {
    const known = shownResources();
    if (!known) return [];
    return searchResources(query, known.entry.rows(), known.off).map((item) => ({ kind: "resource" as const, ...item }));
  };
  const options = (): Option[] => {
    if (!open()) return [];
    const q = mention()!.query.toLowerCase();
    return [
      ...mentionAgents(app.state.agents)
        .filter((agent) => agent.id.toLowerCase().includes(q))
        .slice(0, 8)
        .map((agent) => ({
          kind: "agent" as const,
          name: agent.id,
          description: "Delegate · " + agent.description,
        })),
      ...files().map((name) => ({ kind: "file" as const, name })),
      // After agents and files, as in the TUI, so a server with many
      // resources never pushes files down; and only once this query's files
      // have come, so a resource never sits where a file is about to go and
      // a quick Enter never picks one in its place.
      ...(loading() ? [] : resourceRows(mention()!.query)),
    ];
  };
  let generation = 0;
  createEffect(() => {
    const active = open(),
      query = mention()?.query ?? "",
      connection = app.state.connection?.id;
    const version = ++generation;
    setCursor(0);
    setFiles([]);
    setError("");
    setLoading(false);
    if (!active || !connection) return;
    setLoading(true);
    const timer = setTimeout(() => {
      void window.haarness
        .api<string[]>(connection, "searchFiles", [{ q: query, limit: 20 }])
        .then((values) => {
          if (version === generation) setFiles(values);
        })
        .catch((reason) => {
          if (version === generation) setError(String(reason));
        })
        .finally(() => {
          if (version === generation) setLoading(false);
        });
    }, 120);
    onCleanup(() => clearTimeout(timer));
  });
  async function choose(option: Option) {
    if (busy() || props.disabled) return;
    const range = mention(),
      connection = app.state.connection?.id,
      key = app.draftKey();
    if (!range || !connection) return;
    const text = props.text;
    setBusy(true);
    setError("");
    try {
      const item: PromptContext =
        option.kind === "resource"
          ? {
              kind: "resource",
              server: option.server,
              uri: option.uri,
              name: option.name,
              ...(option.description ? { description: option.description } : {}),
              ...(option.mimeType ? { mimeType: option.mimeType } : {}),
            }
          : option.kind === "agent"
          ? { kind: "agent" as const, name: option.name }
          : await window.haarness
              .files<FileContent>(connection, "read", { path: option.name })
              .then((file) => {
                if (file.encoding !== "utf8")
                  throw Error(
                    "This is a binary file. Use Attach files for images and other media.",
                  );
                return fileContext(file.path, file.content);
              });
      // A delayed remote read must never attach to a different conversation.
      if (app.draftKey() !== key || app.state.connection?.id !== connection)
        return;
      await app.addPromptContext(item, key);
      if (props.text === text && app.draftKey() === key) {
        const next = text.slice(0, range.from) + text.slice(range.to);
        props.change(next);
        queueMicrotask(() => {
          const input = props.textarea();
          input.focus();
          input.setSelectionRange(range.from, range.from);
          setCaret(range.from);
        });
      }
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  }
  const syncCaret = () => setCaret(props.textarea()?.selectionStart ?? 0);
  let chips: HTMLDivElement | undefined;
  /** A chip's preview closes with Escape or a click anywhere else, and the
   * keyboard returns to its chip. */
  const closePreviews = (keep?: Element | null) => {
    for (const details of chips?.querySelectorAll<HTMLDetailsElement>("details[open]") ?? [])
      if (!keep || !details.contains(keep)) details.open = false;
  };
  onMount(() => {
    const away = (event: PointerEvent) => closePreviews(event.target as Element);
    document.addEventListener("pointerdown", away);
    onCleanup(() => document.removeEventListener("pointerdown", away));
  });
  /**
   * The last paste put back as text. The message box's own Undo takes the
   * text out, and then the chip comes back; Redo puts the text in again and
   * takes the chip away, so the paste is never lost between the two.
   */
  let unpasted:
    | { item: Extract<PromptContext, { kind: "paste" }>; key: string; before: string; after: string }
    | undefined;
  /** A paste back in the message box as text, where the caret was. */
  function editAsText(item: Extract<PromptContext, { kind: "paste" }>) {
    if (props.disabled) return;
    const input = props.textarea();
    const before = input.value;
    props.insert(item.text);
    unpasted = { item, key: app.draftKey(), before, after: input.value };
    void app.run(() => app.removePromptContext(item));
  }
  onMount(() => {
    const input = props.textarea();
    const history = (event: Event) => {
      const kind = (event as InputEvent).inputType;
      if (!unpasted || (kind !== "historyUndo" && kind !== "historyRedo")) return;
      const { item, key, before, after } = unpasted;
      if (key !== app.draftKey()) return void (unpasted = undefined);
      const chip = app.promptContext().some((value) => contextKey(value) === contextKey(item));
      if (kind === "historyUndo" && input.value === before && !chip)
        void app.run(() => app.addPromptContext(item, key));
      else if (kind === "historyRedo" && input.value === after && chip)
        void app.run(() => app.removePromptContext(item));
    };
    input.addEventListener("input", history);
    onCleanup(() => input.removeEventListener("input", history));
  });
  onMount(() => {
    const input = props.textarea();
    const update = () => queueMicrotask(syncCaret);
    const outside = (event: Event) => {
      if (
        event.target !== input &&
        !(event.target as HTMLElement)?.closest?.(".mention-popover")
      )
        setDismissed(signature());
    };
    const keydown = (event: KeyboardEvent) => {
      if (!open() || event.isComposing) return;
      if (
        !["ArrowDown", "ArrowUp", "Enter", "Tab", "Escape"].includes(
          event.key,
        )
      )
        return;
      if (event.shiftKey || event.metaKey || event.ctrlKey || event.altKey)
        return;
      event.preventDefault();
      event.stopImmediatePropagation();
      if (event.key === "Escape") {
        setDismissed(signature());
        return;
      }
      const values = options();
      if (!values.length) return;
      if (event.key === "ArrowDown" || event.key === "ArrowUp")
        setCursor(
          (value) =>
            (value + (event.key === "ArrowDown" ? 1 : -1) + values.length) %
            values.length,
        );
      else void choose(values[cursor()] ?? values[0]);
    };
    for (const event of ["input", "click", "keyup", "select"])
      input.addEventListener(event, update);
    input.addEventListener("keydown", keydown, true);
    document.addEventListener("focusin", outside);
    onCleanup(() => {
      for (const event of ["input", "click", "keyup", "select"])
        input.removeEventListener(event, update);
      input.removeEventListener("keydown", keydown, true);
      document.removeEventListener("focusin", outside);
    });
  });
  createEffect(() => {
    const input = props.textarea();
    if (!input) return;
    if (open()) {
      input.setAttribute("aria-controls", "prompt-mentions");
      input.setAttribute(
        "aria-activedescendant",
        `prompt-mention-${cursor()}`,
      );
      document
        .getElementById(`prompt-mention-${cursor()}`)
        ?.scrollIntoView({ block: "nearest" });
    } else if (input.getAttribute("aria-controls") === "prompt-mentions") {
      input.removeAttribute("aria-controls");
      input.removeAttribute("aria-activedescendant");
    }
  });
  return (
    <>
      <Show when={app.promptContext().length}>
        <div
          ref={chips}
          class="prompt-context-items"
          aria-label={t("Attached context")}
        >
          <For each={app.promptContext()}>
            {(item) => (
              <div class="prompt-context-item" data-kind={item.kind}>
                <Icon
                  name={
                    item.kind === "file"
                      ? "file"
                      : item.kind === "element"
                        ? "pick"
                        : item.kind === "paste"
                          ? "paste"
                          : item.kind === "resource"
                            ? "file-text"
                            : "agents"
                  }
                  size="sm"
                />
                <details
                  onToggle={(event) => {
                    if (event.currentTarget.open)
                      closePreviews(event.currentTarget);
                  }}
                  onKeyDown={(event) => {
                    if (event.key !== "Escape" || !event.currentTarget.open)
                      return;
                    event.preventDefault();
                    event.stopPropagation();
                    event.currentTarget.open = false;
                    event.currentTarget.querySelector("summary")?.focus();
                  }}
                >
                  <summary title={chipTitle(item)}>
                    {chipLabel(item, app.promptContext())}
                    {item.kind === "file" && item.unsaved ? ` · ${t("unsaved")}` : ""}
                  </summary>
                  <div
                    class="prompt-context-preview"
                    classList={{ "paste-preview": item.kind === "paste" }}
                  >
                    <Switch
                      fallback={
                        <p>
                          {t("Requests delegation to this agent, using the task tool and normal permissions.")}
                        </p>
                      }
                    >
                      <Match when={item.kind === "paste" && item}>
                        {(paste) => (
                          <>
                            <pre tabIndex={0} aria-label={chipLabel(paste())}>
                              {paste().text}
                            </pre>
                            <div class="paste-preview-actions">
                              <button
                                type="button"
                                disabled={props.disabled}
                                onClick={() => editAsText(paste())}
                              >
                                {t("Edit as text")}
                              </button>
                            </div>
                          </>
                        )}
                      </Match>
                      <Match when={item.kind === "file" && item}>
                        {(file) => (
                          <>
                            <small>{t("Captured file context")}</small>
                            <pre>{file().content}</pre>
                          </>
                        )}
                      </Match>
                      <Match when={item.kind === "element" && item}>
                        {(element) => <ElementPreview item={element()} />}
                      </Match>
                      <Match when={item.kind === "resource" && item}>
                        {(resource) => <ResourcePreview item={resource()} />}
                      </Match>
                    </Switch>
                  </div>
                </details>
                <button
                  type="button"
                  class="icon-button"
                  disabled={props.disabled}
                  aria-label={t("Remove context {name}", { name: chipLabel(item, app.promptContext()) })}
                  // Focus stays in the message box, so the next paste still
                  // reaches it.
                  onMouseDown={(event) => event.preventDefault()}
                  onClick={() => {
                    const key = contextKey(item);
                    void app.run(() => app.removePromptContext(item));
                    // Removed from the keyboard, the button would take the
                    // focus with it.
                    if (chips?.contains(document.activeElement))
                      queueMicrotask(() => {
                        if (!app.promptContext().some((value) => contextKey(value) === key))
                          props.textarea()?.focus({ preventScroll: true });
                      });
                  }}
                >
                  <Icon name="close" size="xs" />
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>
      <Show when={open()}>
        <div class="mention-popover popover" aria-busy={busy() || loading()}>
          <div
            id="prompt-mentions"
            role="listbox"
            aria-label={
              shownResources()?.entry.rows().length
                ? t("Files, agents and resources")
                : t("Files and agents")
            }
            class="mention-options"
          >
            <For each={options()}>
              {(option, index) => (
                <div
                  id={`prompt-mention-${index()}`}
                  role="option"
                  aria-selected={cursor() === index()}
                  aria-disabled={busy()}
                  data-kind={option.kind}
                  classList={{ highlighted: cursor() === index() }}
                  onPointerMove={() => setCursor(index())}
                  onPointerDown={(event) => event.preventDefault()}
                  onClick={() => void choose(option)}
                >
                  <Icon
                    name={option.kind === "file" ? "file" : option.kind === "resource" ? "file-text" : "agents"}
                    size="sm"
                  />
                  <span>
                    <strong>{option.name}</strong>
                    <Show
                      when={option.kind === "resource" ? option.description || option.uri : option.description}
                    >
                      {(detail) => <small dir="auto">{detail()}</small>}
                    </Show>
                  </span>
                  <small class="mention-kind" title={option.kind === "resource" ? t("MCP server") : undefined}>
                    {option.kind === "resource" ? option.server : option.kind === "agent" ? t("Agent") : t("File")}
                  </small>
                </div>
              )}
            </For>
            <Show when={!options().length && !loading()}>
              <p class="muted">{t("No matching files or agents.")}</p>
            </Show>
          </div>
          <Show when={loading()}>
            <p class="muted" role="status">
              {t("Searching files…")}
            </p>
          </Show>
          <Show when={error()}>
            <p role="alert">{error()}</p>
          </Show>
        </div>
      </Show>
    </>
  );
}
