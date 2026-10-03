import { Preview } from "./preview";
import { PreviewContent } from "./PreviewContent";
import { t } from "./i18n";
import { For, Show, createSignal, createEffect, onCleanup } from "solid-js";
import type { FileContent, FileEntry } from "../shared";
import { useApp } from "./state";
import { Button, Icon, ask, confirm, Empty } from "./ui";
import { Editor, type EditorSession } from "./Editor";
import { fileContext } from "./prompt-context";
type OpenFile = FileContent & { editor: EditorSession; mode?: "preview" | "code"; pin?: string };
const emptyHash =
  "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
export function Explorer() {
  const app = useApp();
  const [version, setVersion] = createSignal(0),
    [query, setQuery] = createSignal(""),
    [results, setResults] = createSignal<string[]>([]),
    [tabs, setTabs] = createSignal<OpenFile[]>([]),
    [selected, setSelected] = createSignal(""),
    [edits, setEdits] = createSignal<Record<string, string>>({});
  let timer: ReturnType<typeof setTimeout>;
  let workspaceGeneration = 0;
  let searchGeneration = 0;
  let selectionGeneration = 0;
  const pendingOpens = new Map<string, Promise<void>>();
  const pendingSaves = new Map<EditorSession, Promise<void>>();
  const file = () => tabs().find((item) => item.path === selected());
  const [selection, setSelection] = createSignal({ from: 0, to: 0 });
  createEffect(() => {
    selected();
    setSelection({ from: 0, to: 0 });
  });
  const canAttach = () =>
    !file()?.pin && file()?.encoding === "utf8" &&
    !app.state.threads.find((thread) => thread.id === app.state.selected)
      ?.isSubagent;
  async function attachContext() {
    const value = file();
    if (!value || !canAttach()) return;
    const key = app.draftKey();
    await app.addPromptContext(
      fileContext(
        value.path,
        edits()[value.path] ?? value.content,
        selection(),
        dirty(value.path),
      ),
      key,
    );
    if (app.draftKey() === key) {
      app.set("panel", "chat");
      app.notify("File context added to your message");
    }
  }
  const dirty = (path: string) =>
    edits()[path] !== undefined &&
    edits()[path] !== tabs().find((item) => item.path === path)?.content;
  const hasEdits = () => tabs().some((item) => dirty(item.path));
  app.onLeaveWorkspace(
    async () =>
      !hasEdits() ||
      (await confirm(
        "Discard unsaved file edits?",
        "Save your files before changing working folders, or confirm to discard these edits.",
      )),
  );
  const beforeUnload = (event: BeforeUnloadEvent) => {
    if (hasEdits()) {
      event.preventDefault();
      event.returnValue = "Unsaved files";
    }
  };
  window.addEventListener("beforeunload", beforeUnload);
  onCleanup(() => window.removeEventListener("beforeunload", beforeUnload));
  createEffect(() => {
    app.state.connection?.id;
    workspaceGeneration++;
    selectionGeneration++;
    pendingOpens.clear();
    setQuery("");
    setResults([]);
    setTabs([]);
    setSelected("");
    setEdits({});
    setVersion((v) => v + 1);
  });
  createEffect(() => {
    app.state.selected;
    selectionGeneration++;
    setTabs((items) => items.filter((item) => !item.pin));
  });
  createEffect(() => {
    const connection = app.state.connection?.id;
    const q = query();
    const current = ++searchGeneration;
    setResults([]);
    clearTimeout(timer);
    if (!q || !connection) {
      setResults([]);
      return;
    }
    timer = setTimeout(
      () =>
        void window.haarness
          .api<string[]>(connection, "searchFiles", [{ q, limit: 100 }])
          .then((items) => {
            if (current === searchGeneration) setResults(items);
          })
          .catch((error) => {
            if (current === searchGeneration) app.set("error", String(error));
          }),
      200,
    );
  });
  onCleanup(() => {
    clearTimeout(timer);
    workspaceGeneration++;
    searchGeneration++;
  });
  function select(path: string) {
    selectionGeneration++;
    setSelected(path);
  }
  async function open(path: string, options?: { mode?: "preview" | "code"; version?: string; preview?: boolean }) {
    const intent = ++selectionGeneration;
    const generation = workspaceGeneration;
    const id = app.state.connection?.id;
    if (!id) return;
    if (options?.preview && dirty(path)) {
      app.notify("Save or discard your edits before opening another preview version.");
      return;
    }
    if (!tabs().some((item) => item.path === path) || options?.preview) {
      const key = JSON.stringify([path, options?.version ?? "current", options?.preview ? intent : 0]);
      let pending = pendingOpens.get(key);
      if (!pending) {
        pending = window.haarness.files<FileContent>(id, options?.version ? "preview" : "read", { path, ...(options?.version ? { hash: options.version } : {}) })
          .then((value) => {
            if (generation !== workspaceGeneration || (options?.preview && intent !== selectionGeneration)) return;
            if (options?.preview && dirty(path)) {
              app.notify("Save or discard your edits before refreshing this preview.");
              return;
            }
            if (options?.preview) setEdits((items) => { const next = { ...items }; delete next[path]; return next; });
            const tab: OpenFile = { ...value, editor: {}, mode: options?.mode ?? (["html", "svg"].includes(Preview.kind(path) ?? "") ? "preview" : "code"), pin: options?.version };
            setTabs((items) => items.some((item) => item.path === path)
              ? items.map((item) => item.path === path ? tab : item) : [...items, tab]);
          });
        pendingOpens.set(key, pending);
      }
      try {
        await pending;
      } catch (error) {
        if (generation === workspaceGeneration) throw error;
      } finally {
        if (pendingOpens.get(key) === pending) pendingOpens.delete(key);
      }
    }
    if (generation === workspaceGeneration && intent === selectionGeneration)
      setSelected(path);
  }
  function save(): Promise<void> {
    const value = file();
    const id = app.state.connection?.id;
    if (!value || !id || value.pin) return Promise.resolve();
    const existing = pendingSaves.get(value.editor);
    if (existing) return existing;
    if (!dirty(value.path)) return Promise.resolve();
    const generation = workspaceGeneration;
    const content = edits()[value.path] ?? value.content;
    const pending = window.haarness.files<FileContent>(id, "write", {
      path: value.path, content, revision: value.revision,
    }).then((result) => {
      if (generation !== workspaceGeneration ||
          !tabs().some((item) => item.editor === value.editor)) return;
      // A save advances the disk revision, never the current editing buffer.
      setTabs((items) => items.map((item) => item.editor === value.editor
        ? { ...item, ...result, editor: value.editor } : item));
      setEdits((items) => {
        if (items[value.path] !== content) return items;
        const next = { ...items };
        delete next[value.path];
        return next;
      });
      setVersion((v) => v + 1);
      app.notify("Saved " + value.path);
    }).catch((error) => {
      if (generation === workspaceGeneration) throw error;
    }).finally(() => pendingSaves.delete(value.editor));
    pendingSaves.set(value.editor, pending);
    return pending;
  }
  createEffect(() => {
    const request = app.state.fileRequest;
    if (!request) return;
    app.set("fileRequest", null);
    if (request.connection !== app.state.connection?.id || (request.thread && request.thread !== app.state.selected)) return;
    void app.run(() =>
      request.action
        ? create(request.action === "folder")
        : open(request.path, request),
    );
  });
  app.registerSearch("editor", () => {
    if (!app.state.connection) return [];
    const action = (run: () => unknown) => () => {
      app.set("panel", "files");
      return run();
    };
    return [
      {
        key: "file-new",
        group: "Editor actions",
        name: t("New file"),
        icon: "file-new",
        action: action(() => create()),
      },
      {
        key: "folder-new",
        group: "Editor actions",
        name: t("New folder"),
        icon: "folder-new",
        action: action(() => create(true)),
      },
      {
        key: "file-refresh",
        group: "Editor actions",
        name: t("Refresh files"),
        icon: "refresh",
        action: action(() => setVersion((value) => value + 1)),
      },
      ...tabs().map((item) => ({
        key: "open-file:" + item.path,
        group: "Open files",
        name: item.path,
        icon: "file" as const,
        action: action(() => open(item.path)),
      })),
      ...(file()
        ? [
            {
              key: "file-save",
              group: "Editor actions",
              name: t("Save file"),
              icon: "check" as const,
              hint: "Mod S",
              detail: file()!.path,
              action: action(save),
            },
            {
              key: "file-rename",
              group: "Editor actions",
              name: t("Rename file"),
              icon: "edit" as const,
              detail: file()!.path,
              action: action(rename),
            },
            {
              key: "file-delete",
              group: "Editor actions",
              name: t("Delete file"),
              icon: "trash" as const,
              detail: file()!.path,
              action: action(remove),
            },
            {
              key: "file-close",
              group: "Editor actions",
              name: t("Close file"),
              icon: "close" as const,
              detail: file()!.path,
              action: action(() => close(file()!.path)),
            },
          ]
        : []),
    ];
  });
  async function close(path: string) {
    const value = tabs().find((item) => item.path === path);
    const generation = workspaceGeneration;
    if (!value) return;
    await pendingSaves.get(value.editor);
    if (
      dirty(path) &&
      !(await confirm(
        "Discard unsaved changes?",
        path + " has changes that have not been saved.",
      ))
    )
      return;
    if (generation !== workspaceGeneration) return;
    setTabs((items) => items.filter((item) => item.editor !== value.editor));
    setEdits((items) => {
      const next = { ...items };
      delete next[path];
      return next;
    });
    if (selected() === path) select(tabs()[0]?.path ?? "");
  }
  async function create(directory = false) {
    const id = app.state.connection?.id;
    const generation = workspaceGeneration;
    if (!id) return;
    const input = await ask(directory ? "New folder" : "New file", [
      { name: "path", label: "Path relative to this folder", required: true },
    ], {
      submit: "Create",
      onSubmit: async (input) => {
        if (generation !== workspaceGeneration) throw Error("The working folder changed. Close this dialog and try again.");
        if (directory)
          await window.haarness.files(id, "action", { action: "mkdir", path: input.path });
        else
          await window.haarness.files(id, "write", { path: input.path, content: "", revision: emptyHash });
      },
    });
    if (!input || generation !== workspaceGeneration) return;
    setVersion((v) => v + 1);
    if (!directory) await open(input.path);
  }
  async function rename() {
    const value = file();
    const id = app.state.connection?.id;
    const generation = workspaceGeneration;
    if (!value || !id) return;
    await ask("Rename file", [
      { name: "target", label: "New path", value: value.path, required: true },
    ], {
      onSubmit: async (input) => {
        if (generation !== workspaceGeneration) throw Error("The working folder changed. Close this dialog and try again.");
        // A rename must not race a pending write to the old path.
        await pendingSaves.get(value.editor);
        await window.haarness.files(id, "action", {
          action: "rename", path: value.path, target: input.target,
        });
        if (generation !== workspaceGeneration) return;
        setTabs((items) => items.map((item) => item.editor === value.editor
          ? { ...item, path: input.target } : item));
        setEdits((items) => {
          const next = { ...items };
          if (next[value.path] !== undefined) {
            next[input.target] = next[value.path];
            delete next[value.path];
          }
          return next;
        });
        select(input.target);
        setVersion((v) => v + 1);
      },
    });
  }
  async function restorePreview() {
    const value = file(), id = app.state.connection?.id;
    const generation = workspaceGeneration;
    const intent = selectionGeneration;
    if (!value?.pin || !id || !await confirm("Restore preview version?", "The current file is backed up before this version replaces it.")) return;
    const current = await window.haarness.files<FileContent>(id, "read", { path: value.path }).catch(() => undefined);
    await window.haarness.files(id, "restore", { path: value.path, hash: value.pin, revision: current?.revision ?? emptyHash });
    if (generation === workspaceGeneration && intent === selectionGeneration) await open(value.path, { preview: true });
  }
  async function remove() {
    const value = file();
    const id = app.state.connection?.id;
    const generation = workspaceGeneration;
    if (!value || !id || !(await confirm("Delete this file?", value.path + " will be deleted from the workspace."))) return;
    await pendingSaves.get(value.editor);
    if (generation !== workspaceGeneration) return;
    await window.haarness.files(id, "action", { action: "delete", path: value.path, confirm: true });
    if (generation !== workspaceGeneration) return;
    setEdits((items) => {
      const next = { ...items };
      delete next[value.path];
      return next;
    });
    await close(value.path);
    setVersion((v) => v + 1);
  }
  return (
    <section class="explorer">
      <div class="panel-toolbar">
        <input
          class="panel-search"
          aria-label={t("Find files")}
          placeholder={t("Find a file…")}
          value={query()}
          onInput={(event) => setQuery(event.currentTarget.value)}
        />
        <Button
          icon="file-new"
          label={t("New file")}
          onClick={() => app.run(() => create())}
        />
        <Button
          icon="folder-new"
          label={t("New folder")}
          onClick={() => app.run(() => create(true))}
        />
        <Button
          icon="refresh"
          label={t("Refresh file tree")}
          onClick={() => setVersion((v) => v + 1)}
        />
      </div>
      <div class="file-workspace">
        <aside class="file-tree">
          <Show
            when={query()}
            fallback={
              <Tree
                path=""
                version={version()}
                hidden={true}
                selected={selected()}
                open={(path) => app.run(() => open(path))}
              />
            }
          >
            <For each={results()}>
              {(path) => (
                <button
                  class="file-entry"
                  title={path}
                  onClick={() => app.run(() => open(path))}
                >
                  <Icon name="file" size="sm" />
                  <span>{path}</span>
                </button>
              )}
            </For>
          </Show>
        </aside>
        <div class="file-preview">
          <Show when={tabs().length}>
            <div class="file-toolbar">
              <div class="file-tabs">
              <For each={tabs()}>
                {(item) => (
                  <div classList={{ active: item.path === selected() }}>
                    <button
                      title={
                        item.path +
                        " · " +
                        Math.round((item.size / 1024) * 10) / 10 +
                        " KB"
                      }
                      onClick={() => select(item.path)}
                    >
                      {item.path.split("/").pop()}
                      {dirty(item.path) ? " •" : ""}
                    </button>
                    <Button
                      icon="close"
                      label={t("Close {name}", { name: item.path })}
                      onClick={() => app.run(() => close(item.path))}
                    />
                  </div>
                )}
              </For>
              </div>
              <Show when={file()}>
                <div class="file-actions">
                  <Show when={Preview.kind(file()!.path) && Preview.kind(file()!.path) !== "image"}>
                    <button aria-pressed={file()?.mode !== "code"} onClick={() => setTabs((items) => items.map((item) => item.path === selected() ? { ...item, mode: "preview" } : item))}>Preview</button>
                    <button aria-pressed={file()?.mode === "code"} onClick={() => setTabs((items) => items.map((item) => item.path === selected() ? { ...item, mode: "code" } : item))}>Code</button>
                  </Show>

                  <Button
                    icon="file"
                    label={
                      selection().from !== selection().to
                        ? "Add selection to context"
                        : "Add file to context"
                    }
                    hint="Command/Ctrl+Shift+L"
                    disabled={!canAttach()}
                    onClick={() => app.run(attachContext)}
                  />
                  <Button
                    icon="edit"
                    label={t("Rename")}
                    onClick={() => app.run(rename)}
                  />
                  <Button
                    icon="trash"
                    label={t("Delete")}
                    onClick={() => app.run(remove)}
                  />
                  <Show when={!app.state.connection?.remote}>
                    <Button
                      icon="folder-open"
                      label={t("Reveal in file manager")}
                      onClick={() =>
                        app.run(() =>
                          window.haarness.reveal(
                            app.state.connection!.id,
                            file()!.path,
                          ),
                        )
                      }
                    />
                  </Show>
                  <button
                    disabled={!dirty(file()!.path)}
                    class="primary save-file"
                    onClick={() => app.run(save)}
                  >
                    {t("Save")}
                  </button>
                </div>
              </Show>
            </div>
          </Show>
          <Show
            when={file()}
            fallback={
              <Empty
                title={t("Your code, close at hand.")}
                detail="Open a file from the explorer to read, edit, or review it."
              />
            }
          >
            {(value) => (
              <>
                <Show when={value().pin}>
                  <div class="preview-version-strip">Saved preview version <button onClick={() => app.run(restorePreview)}>Restore this version</button><button onClick={() => app.run(() => open(value().path, { preview: true }))}>Show current</button></div>
                </Show>
                <Show when={Preview.kind(value().path) && value().mode !== "code"} fallback={<>
                <Show
                  when={value().encoding === "utf8"}
                  fallback={
                    <div class="media-preview">
                      <Show
                        when={value().mime.startsWith("image/")}
                        fallback={
                          <Show
                            when={value().mime.startsWith("audio/")}
                            fallback={
                              <Show
                                when={value().mime.startsWith("video/")}
                                fallback={
                                  <Empty
                                    title={t("Binary file")}
                                    detail={`${value().mime} · ${value().size} bytes`}
                                  />
                                }
                              >
                                <video
                                  controls
                                  src={`data:${value().mime};base64,${value().content}`}
                                />
                              </Show>
                            }
                          >
                            <audio
                              controls
                              src={`data:${value().mime};base64,${value().content}`}
                            />
                          </Show>
                        }
                      >
                        <img
                          alt={value().path}
                          src={`data:${value().mime};base64,${value().content}`}
                        />
                      </Show>
                    </div>
                  }
                >
                  <Editor
                    session={value().editor}
                    path={value().path}
                    value={edits()[value().path] ?? value().content}
                    readonly={!!value().pin}
                    wrap={app.state.preferences.editorWrap}
                    onChange={(text) =>
                      setEdits((items) => ({ ...items, [value().path]: text }))
                    }
                    onSave={() => app.run(save)}
                    onSelection={setSelection}
                    onAddContext={() => void app.run(attachContext)}
                  />
                </Show>
                </>}><PreviewContent file={{ ...value(), content: edits()[value().path] ?? value().content }} /></Show>
              </>
            )}
          </Show>
        </div>
      </div>
    </section>
  );
}
function Tree(props: {
  path: string;
  version: number;
  hidden: boolean;
  selected: string;
  open: (path: string) => void;
}) {
  const app = useApp();
  const [entries, setEntries] = createSignal<FileEntry[]>([]),
    [error, setError] = createSignal("");
  let generation = 0;
  createEffect(() => {
    props.version;
    const id = ++generation;
    void window.haarness
      .files<{ entries: FileEntry[]; truncated: boolean }>(
        app.state.connection!.id,
        "list",
        { path: props.path, hidden: props.hidden },
      )
      .then((result) => {
        if (id !== generation) return;
        setEntries(result.entries);
        setError(
          result.truncated
            ? "This older server returned a partial listing. Use file search to find other files."
            : "",
        );
      })
      .catch((error) => { if (id === generation) setError(String(error)); });
  });
  onCleanup(() => generation++);
  return (
    <div role="tree">
      <For each={entries()}>
        {(item) => <Entry entry={item} {...props} />}
      </For>
      <Show when={error()}>
        <p class="small danger">{error()}</p>
      </Show>
    </div>
  );
}
function Entry(props: {
  entry: FileEntry;
  version: number;
  hidden: boolean;
  selected: string;
  open: (path: string) => void;
}) {
  const [expanded, setExpanded] = createSignal(false);
  return (
    <div
      role="treeitem"
      aria-expanded={props.entry.directory ? expanded() : undefined}
    >
      <button
        class="file-entry"
        classList={{ selected: props.selected === props.entry.path }}
        title={props.entry.path}
        onClick={() =>
          props.entry.directory
            ? setExpanded((v) => !v)
            : props.open(props.entry.path)
        }
      >
        <Icon
          name={
            props.entry.directory ? (expanded() ? "down" : "chevron") : "file"
          }
          size="sm"
        />
        <span>{props.entry.name}</span>
        <Show when={props.entry.symlink}>
          <span class="entry-symlink" title={t("Symbolic link")}>
            <Icon name="link" size="xs" />
          </span>
        </Show>
      </button>
      <Show when={expanded()}>
        <div class="tree-children">
          <Tree
            path={props.entry.path}
            version={props.version}
            hidden={props.hidden}
            selected={props.selected}
            open={props.open}
          />
        </div>
      </Show>
    </div>
  );
}
