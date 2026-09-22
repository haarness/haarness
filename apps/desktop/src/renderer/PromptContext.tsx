import { t } from "./i18n";
import {
  createEffect,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from "solid-js";
import { useApp } from "./state";
import { Icon } from "./icons";
import type { FileContent } from "../shared";
import {
  activeMention,
  contextLabel,
  fileContext,
  mentionAgents,
} from "./prompt-context";
import "./prompt-context.css";

type Option = { kind: "file" | "agent"; name: string; description?: string };
export function PromptContextControls(props: {
  text: string;
  textarea: () => HTMLTextAreaElement;
  change: (value: string) => void;
  disabled: boolean;
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
      const item =
        option.kind === "agent"
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
        <div class="prompt-context-items" aria-label={t("Attached context")}>
          <For each={app.promptContext()}>
            {(item) => (
              <div class="prompt-context-item">
                <Icon
                  name={item.kind === "file" ? "file" : "agents"}
                  size="sm"
                />
                <details>
                  <summary title={contextLabel(item)}>
                    {contextLabel(item)}
                    {item.kind === "file" && item.unsaved ? " · unsaved" : ""}
                  </summary>
                  <div class="prompt-context-preview">
                    <Show
                      when={item.kind === "file"}
                      fallback={
                        <p>
                          Requests delegation to this agent, using the task
                          tool and normal permissions.
                        </p>
                      }
                    >
                      <small>{t("Captured file context")}</small>
                      <pre>{item.kind === "file" ? item.content : ""}</pre>
                    </Show>
                  </div>
                </details>
                <button
                  type="button"
                  class="icon-button"
                  disabled={props.disabled}
                  aria-label={t("Remove context {name}", { name: contextLabel(item) })}
                  onClick={() =>
                    void app.run(() => app.removePromptContext(item))
                  }
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
            aria-label={t("Files and agents")}
            class="mention-options"
          >
            <For each={options()}>
              {(option, index) => (
                <div
                  id={`prompt-mention-${index()}`}
                  role="option"
                  aria-selected={cursor() === index()}
                  aria-disabled={busy()}
                  classList={{ highlighted: cursor() === index() }}
                  onPointerMove={() => setCursor(index())}
                  onPointerDown={(event) => event.preventDefault()}
                  onClick={() => void choose(option)}
                >
                  <Icon
                    name={option.kind === "file" ? "file" : "agents"}
                    size="sm"
                  />
                  <span>
                    <strong>{option.name}</strong>
                    <Show when={option.description}>
                      <small>{option.description}</small>
                    </Show>
                  </span>
                  <small>{option.kind === "agent" ? "Agent" : "File"}</small>
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
