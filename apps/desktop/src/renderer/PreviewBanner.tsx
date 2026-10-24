import { t } from "./i18n";
import {
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
} from "solid-js";
import { useApp } from "./state";
import { currentPreview, previewInfo } from "./preview-actions";
import { PreviewContent } from "./PreviewContent";
import type { FileContent } from "../shared";
import { diffLines } from "diff";
import { confirm } from "./ui";
const emptyHash =
  "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
export function PreviewBanner(props: { metadata: unknown }) {
  const app = useApp();
  const info = createMemo(() => previewInfo(props.metadata));
  const [mode, setMode] = createSignal<"preview" | "code" | "compare">();
  const [version, setVersion] = createSignal<FileContent>();
  const [current, setCurrent] = createSignal<{ revision: string | null }>();
  const [live, setLive] = createSignal<FileContent>();
  let loading = 0;
  const [error, setError] = createSignal("");
  const [backup, setBackup] = createSignal<string>();
  const [busy, setBusy] = createSignal(false);
  let generation = 0;
  async function refresh() {
    const value = info(),
      connection = app.state.connection?.id;
    if (!value || !connection) return;
    const sequence = ++generation;
    try {
      const result = await currentPreview(connection, value.path);
      if (sequence === generation && app.state.connection?.id === connection)
        setCurrent(result);
    } catch {
      if (sequence === generation)
        setError(t("The current file is unavailable. Try opening it again."));
    }
  }
  async function expand(next: "preview" | "code" | "compare") {
    void refresh();
    if (mode() === next) {
      setMode(undefined);
      setVersion(undefined);
      setLive(undefined);
      loading++;
      return;
    }
    const value = info(),
      connection = app.state.connection?.id;
    if (!value || !connection) return;
    const sequence = ++loading;
    setMode(next);
    setError("");
    try {
      const saved = await window.haarness.files<FileContent>(
        connection,
        "preview",
        { path: value.path, hash: value.version },
      );
      const live =
        next === "compare" && current()?.revision !== null
          ? await window.haarness.files<FileContent>(connection, "read", {
              path: value.path,
            })
          : undefined;
      if (sequence !== loading || app.state.connection?.id !== connection)
        return;
      setVersion(saved);
      setLive(live);
      if (live) setCurrent({ revision: live.revision });
    } catch {
      if (sequence === loading)
        setError(
          t("This preview could not be loaded. Open the current file to continue."),
        );
    }
  }
  createEffect(() => {
    info();
    app.state.connection?.id;
    void refresh();
  });
  const off = window.haarness.onEvent((event) => {
    if (
      event.kind !== "server" ||
      event.connection !== app.state.connection?.id
    )
      return;
    const data = event.data as { type: string; payload?: { path?: string } };
    if (
      (data.type === "files.updated" && data.payload?.path === info()?.path) ||
      data.type === "turn.finished"
    )
      void refresh();
  });
  const focused = () => void refresh();
  window.addEventListener("focus", focused);
  onCleanup(() => {
    generation++;
    loading++;
    off();
    window.removeEventListener("focus", focused);
  });
  const state = () =>
    !current()
      ? t("Checking version…")
      : current()?.revision === null
        ? t("Deleted")
        : current()?.revision === info()?.version
          ? t("Current")
          : t("Earlier version");
  function open() {
    void refresh();
    const value = info(),
      connection = app.state.connection?.id;
    if (!value || !connection) return;
    app.set("fileRequest", {
      connection,
      thread: app.state.selected,
      path: value.path,
      mode: "preview",
      preview: true,
      fromHome: true,
      version: value.version,
      sequence: Date.now(),
    });
  }
  async function restore(hash: string) {
    const value = info(),
      connection = app.state.connection?.id;
    const thread = app.state.selected;
    if (!value || !connection || busy()) return;
    if (
      !(await confirm(
        t("Restore preview version?"),
        t("The current file is backed up before this version replaces it. Unsaved editor changes are kept."),
        { submit: t("Restore version") },
      ))
    )
      return;
    setBusy(true);
    try {
      const result = await window.haarness.files<{
        file: FileContent;
        backup: string;
      }>(connection, "restore", {
        path: value.path,
        hash,
        revision: current()?.revision ?? emptyHash,
      });
      if (connection !== app.state.connection?.id || thread !== app.state.selected) return;
      setBackup(result.backup);
      setCurrent({ revision: result.file.revision });
      setLive(result.file);
      app.set("fileRequest", {
        connection,
        thread,
        path: value.path,
        mode: "preview",
        preview: true,
        fromHome: true,
        sequence: Date.now(),
      });
    } finally {
      setBusy(false);
    }
  }
  const comparison = createMemo(() => {
    if (version()?.encoding === "base64" || live()?.encoding === "base64")
      return [
        {
          added: false,
          removed: false,
          value: "Binary image changed. Use Preview to inspect each version.",
        },
      ];
    return (
      diffLines(version()?.content ?? "", live()?.content ?? "", {
        timeout: 100,
      }) ?? [
        {
          added: false,
          removed: false,
          value:
            "This comparison is too large to display. Use Code to inspect each version.",
        },
      ]
    );
  });
  return (
    <Show when={info()}>
      {(value) => (
        <section class="preview-banner">
          <div class="preview-banner-heading">
            <button class="preview-title" onClick={open}>
              <strong>{value().title}</strong>
              <span>{value().path}</span>
            </button>
            <span class="preview-status">{state()}</span>
          </div>
          <div class="preview-banner-actions">
            <button onClick={open}>{t("Open preview")}</button>
            <button
              aria-pressed={mode() === "preview"}
              onClick={() => void expand("preview")}
            >
              {t("Preview")}
            </button>
            <button
              aria-pressed={mode() === "code"}
              onClick={() => void expand("code")}
            >
              {t("Code")}
            </button>
            <Show when={current() && current()?.revision !== value().version}>
              <button onClick={() => void expand("compare")}>{t("Compare")}</button>
              <button
                disabled={busy()}
                onClick={() => app.run(() => restore(value().version))}
              >
                {t("Restore")}
              </button>
            </Show>
            <Show when={backup()}>
              {(hash) => (
                <button
                  disabled={busy()}
                  onClick={() => app.run(() => restore(hash()))}
                >
                  {t("Undo restore")}
                </button>
              )}
            </Show>
          </div>
          <Show when={error()}>
            <p class="preview-notice">
              {error()}{" "}
              <button
                onClick={() => {
                  const connection = app.state.connection?.id;
                  if (connection)
                    app.set("fileRequest", {
                      connection,
                      path: value().path,
                      mode: "preview",
                      preview: true,
                      fromHome: true,
                      sequence: Date.now(),
                    });
                }}
              >
                {t("Open current")}
              </button>
            </p>
          </Show>
          <Show when={mode() && version()}>
            {(file) => (
              <div class="preview-inline">
                <Show
                  when={mode() === "preview"}
                  fallback={
                    <Show
                      when={mode() === "compare"}
                      fallback={
                        <pre class="preview-code">
                          {file().encoding === "utf8"
                            ? file().content
                            : t("Binary image")}
                        </pre>
                      }
                    >
                      <pre class="preview-code">
                        {comparison().map((part) => (
                          <span
                            class={
                              part.added
                                ? "preview-added"
                                : part.removed
                                  ? "preview-removed"
                                  : ""
                            }
                          >
                            {part.value}
                          </span>
                        ))}
                      </pre>
                    </Show>
                  }
                >
                  <PreviewContent file={file()} fit />
                </Show>
              </div>
            )}
          </Show>
        </section>
      )}
    </Show>
  );
}
