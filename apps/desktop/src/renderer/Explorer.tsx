import { Preview } from "./preview";
import { PreviewContent } from "./PreviewContent";
import { t } from "./i18n";
import { For, Show, batch, createSignal, createEffect, onCleanup } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type { FileContent, FileEntry } from "../shared";
import { useApp } from "./state";
import { Button, Icon, ask, confirm, Empty } from "./ui";
import { Editor, type EditorSession } from "./Editor";
import { fileContext } from "./prompt-context";
import { shortcutLabel, shortcutText } from "./search";
import { registerCyclePanel } from "./tab-cycle";
/** `changed` is the file on disk when it moved on under unsaved edits. */
type OpenFile = FileContent & { editor: EditorSession; mode?: "preview" | "code"; pin?: string; changed?: FileContent };
const emptyHash =
  "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
export function Explorer() {
  const app = useApp();
  const [version, setVersion] = createSignal(0),
    [query, setQuery] = createSignal(""),
    [results, setResults] = createSignal<string[]>([]),
    [tabs, setTabs] = createSignal<OpenFile[]>([]),
    [selected, setSelected] = createSignal(""),
    [edits, setEdits] = createSignal<Record<string, string>>({}),
    // Owned here, not by each row, so folders stay open while the tree is
    // listed again after a save, a refresh or a change on disk.
    [expanded, setExpanded] = createSignal<ReadonlySet<string>>(new Set());
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
      app.notify(t("File context added to your message"));
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
        t("Discard unsaved file edits?"),
        t("Save your files before changing working folders, or discard these edits."),
        { submit: t("Discard edits"), danger: true },
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
    setExpanded(new Set<string>());
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
  // Ctrl+Tab in the Files panel steps its open files.
  onCleanup(
    registerCyclePanel({
      id: "files",
      tabs: () => tabs().map((item) => item.path),
      active: () => selected(),
      select,
    }),
  );
  function select(path: string) {
    selectionGeneration++;
    setSelected(path);
  }
  function toggle(path: string) {
    setExpanded((items) => {
      const next = new Set(items);
      if (!next.delete(path)) next.add(path);
      return next;
    });
  }
  /** Open the folders above a path so the file shows in the tree. */
  function reveal(path: string) {
    setExpanded((items) => {
      const next = new Set(items);
      const parts = path.split("/");
      for (let index = 1; index < parts.length; index++)
        next.add(parts.slice(0, index).join("/"));
      return next;
    });
  }
  /** Take the disk's version of a tab; `keep` keeps its unsaved edits on top. */
  function adopt(editor: EditorSession, disk: FileContent, keep = false) {
    batch(() => {
      setTabs((items) => items.map((item) => item.editor === editor
        ? { ...item, ...disk, editor, mode: item.mode, changed: undefined } : item));
      if (!keep)
        setEdits((items) => {
          const next = { ...items };
          delete next[disk.path];
          return next;
        });
    });
  }
  /**
   * The file moved on under an open tab. A tab without unsaved edits follows
   * the disk; one with edits keeps them and asks whether to reload.
   */
  function diverged(editor: EditorSession, disk: FileContent) {
    const value = tabs().find((item) => item.editor === editor);
    if (!value || value.path !== disk.path) return;
    if (!dirty(value.path) || edits()[value.path] === disk.content) adopt(editor, disk);
    else if (value.changed?.revision !== disk.revision)
      setTabs((items) => items.map((item) => item.editor === editor ? { ...item, changed: disk } : item));
  }
  async function recheck(path: string) {
    const id = app.state.connection?.id;
    const value = tabs().find((item) => item.path === path);
    if (!id || !value || value.pin || pendingSaves.has(value.editor)) return;
    const generation = workspaceGeneration;
    const disk = await window.haarness
      .files<FileContent>(id, "read", { path })
      .catch(() => undefined);
    const current = tabs().find((item) => item.editor === value.editor);
    // A save or reload that finished meanwhile knows better than this read.
    if (!disk || generation !== workspaceGeneration || !current ||
        current.revision !== value.revision || pendingSaves.has(value.editor)) return;
    if (disk.revision !== current.revision) diverged(value.editor, disk);
    else if (current.changed)
      setTabs((items) => items.map((item) => item.editor === value.editor ? { ...item, changed: undefined } : item));
  }
  // The agent, a terminal, another window or another editor can change files
  // under the panel: list the tree again and bring open files up to date.
  let changes: ReturnType<typeof setTimeout> | undefined;
  function changedOnDisk() {
    clearTimeout(changes);
    changes = setTimeout(() => {
      setVersion((v) => v + 1);
      for (const item of tabs()) void recheck(item.path);
    }, 250);
  }
  const off = window.haarness.onEvent((event) => {
    if (event.kind !== "server" || event.connection !== app.state.connection?.id) return;
    const data = event.data as {
      type: string;
      payload?: { data?: { type?: string; name?: string; is_error?: boolean } };
    };
    const result = data.type === "item.created" ? data.payload?.data : undefined;
    if (
      data.type === "files.updated" ||
      data.type === "turn.finished" ||
      (result?.type === "tool_result" && !result.is_error &&
        /write|edit|patch|apply|bash|shell|move|delete/.test(result.name ?? ""))
    )
      changedOnDisk();
  });
  window.addEventListener("focus", changedOnDisk);
  onCleanup(() => {
    off();
    window.removeEventListener("focus", changedOnDisk);
    clearTimeout(changes);
  });
  async function open(path: string, options?: { mode?: "preview" | "code"; version?: string; preview?: boolean }) {
    const intent = ++selectionGeneration;
    const generation = workspaceGeneration;
    const id = app.state.connection?.id;
    if (!id) return;
    if (options?.preview && dirty(path)) {
      app.notify(t("Save or discard your edits before opening another preview version."));
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
              app.notify(t("Save or discard your edits before refreshing this preview."));
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
    } else void recheck(path);
    if (generation === workspaceGeneration && intent === selectionGeneration)
      setSelected(path);
  }
  /** The bar asking whether to reload a file that changed on disk. */
  let changedBar: HTMLDivElement | undefined;
  /** Save can't go ahead until the reader chooses Reload or Keep mine, so
   * point at that choice rather than appear to do nothing. */
  function pointAtChangedBar() {
    const bar = changedBar;
    if (!bar?.isConnected) return;
    bar.classList.remove("attention");
    void bar.offsetWidth; // Restart the flash on every press.
    bar.classList.add("attention");
    bar.focus();
  }
  /** Once the choice is made, the keyboard goes back to the text it was about. */
  function backToEditor() {
    const bar = changedBar;
    const preview = bar?.closest(".file-preview");
    if (!preview || !bar!.contains(document.activeElement)) return;
    setTimeout(() => preview.querySelector<HTMLElement>(".cm-content")?.focus());
  }
  function save(): Promise<void> {
    const value = file();
    const id = app.state.connection?.id;
    if (!value || !id || value.pin) return Promise.resolve();
    const existing = pendingSaves.get(value.editor);
    if (existing) return existing;
    if (!dirty(value.path)) return Promise.resolve();
    if (value.changed) {
      pointAtChangedBar();
      return Promise.resolve();
    }
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
      app.notify(t("Saved {path}", { path: value.path }));
    }).catch(async (error) => {
      if (generation !== workspaceGeneration) return;
      // Someone else wrote the file since it was read. Offer the choice
      // rather than an error that only says to reload.
      const disk = await window.haarness
        .files<FileContent>(id, "read", { path: value.path })
        .catch(() => undefined);
      if (generation !== workspaceGeneration) return;
      if (disk && disk.revision !== value.revision) return diverged(value.editor, disk);
      throw error;
    }).finally(() => pendingSaves.delete(value.editor));
    pendingSaves.set(value.editor, pending);
    return pending;
  }
  /** Where the editor puts the cursor for a link to `path:line`, once per click. */
  const [jump, setJump] = createSignal<{ path: string; line: number }>();
  createEffect(() => {
    const request = app.state.fileRequest;
    if (!request) return;
    app.set("fileRequest", null);
    if (request.connection !== app.state.connection?.id || (request.thread && request.thread !== app.state.selected)) return;
    void app.run(() =>
      request.action
        ? create(request.action === "folder")
        : request.link
          ? openLink(request.path, request.line)
          : open(request.path, request),
    );
  });
  /** A link's target: a file opens (at its line), a folder opens in the tree,
   * and anything else is named as missing rather than shown as a server error. */
  async function openLink(path: string, line?: number) {
    const id = app.state.connection?.id;
    const generation = workspaceGeneration;
    // A link to the project itself has nothing more to open than the panel.
    if (!id || !path) return;
    try {
      await open(path);
    } catch (error) {
      const folder = await window.haarness
        .files(id, "list", { path })
        .then(() => true, () => false);
      if (generation !== workspaceGeneration) return;
      if (folder) return reveal(path + "/");
      // The server's words for a missing file and for a link that leads out of the project.
      const message = error instanceof Error ? error.message : String(error);
      if (/no longer exists/i.test(message))
        app.notify(t("{path} was not found in this project.", { path }), "blocked");
      else if (/escapes the workspace/i.test(message))
        app.notify(t("Only files in this project open from a link."), "blocked");
      else throw error;
      return;
    }
    if (line && generation === workspaceGeneration && selected() === path)
      setJump({ path, line });
  }
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
              hint: shortcutLabel("Mod+S", app.state.platform),
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
        t("Discard unsaved changes?"),
        t("{path} has changes that have not been saved.", { path }),
        { submit: t("Discard changes"), danger: true },
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
    const input = await ask(directory ? t("New folder") : t("New file"), [
      { name: "path", label: t("Path relative to this folder"), required: true },
    ], {
      submit: t("Create"),
      onSubmit: async (input) => {
        if (generation !== workspaceGeneration) throw Error(t("The working folder changed. Close this dialog and try again."));
        if (directory)
          await window.haarness.files(id, "action", { action: "mkdir", path: input.path });
        else
          await window.haarness.files(id, "write", { path: input.path, content: "", revision: emptyHash });
      },
    });
    if (!input || generation !== workspaceGeneration) return;
    reveal(input.path);
    setVersion((v) => v + 1);
    if (!directory) await open(input.path);
  }
  async function rename() {
    const value = file();
    const id = app.state.connection?.id;
    const generation = workspaceGeneration;
    if (!value || !id) return;
    await ask(t("Rename file"), [
      { name: "target", label: t("New path"), value: value.path, required: true },
    ], {
      onSubmit: async (input) => {
        if (generation !== workspaceGeneration) throw Error(t("The working folder changed. Close this dialog and try again."));
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
  async function reload() {
    const value = file(), id = app.state.connection?.id;
    const generation = workspaceGeneration;
    if (!value || !id) return;
    const disk = await window.haarness.files<FileContent>(id, "read", { path: value.path });
    if (generation !== workspaceGeneration) return;
    backToEditor();
    adopt(value.editor, disk);
  }
  /** Keep the edits; the next save replaces the file on disk. */
  function keepMine() {
    const value = file();
    if (!value?.changed) return;
    backToEditor();
    adopt(value.editor, value.changed, true);
  }
  async function restorePreview() {
    const value = file(), id = app.state.connection?.id;
    const generation = workspaceGeneration;
    const intent = selectionGeneration;
    if (!value?.pin || !id || !await confirm(t("Restore preview version?"), t("The current file is backed up before this version replaces it."), { submit: t("Restore version") })) return;
    const current = await window.haarness.files<FileContent>(id, "read", { path: value.path }).catch(() => undefined);
    await window.haarness.files(id, "restore", { path: value.path, hash: value.pin, revision: current?.revision ?? emptyHash });
    if (generation === workspaceGeneration && intent === selectionGeneration) await open(value.path, { preview: true });
  }
  async function remove() {
    const value = file();
    const id = app.state.connection?.id;
    const generation = workspaceGeneration;
    if (!value || !id || !(await confirm(t("Delete this file?"), t("{path} will be deleted from the workspace.", { path: value.path }), { submit: t("Delete file"), danger: true }))) return;
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
    <section class="explorer" data-cycle-panel="files">
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
                expanded={(path) => expanded().has(path)}
                toggle={toggle}
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
                      data-cycle-tab={item.path}
                      aria-current={item.path === selected() ? "true" : undefined}
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
                    <button aria-pressed={file()?.mode !== "code"} onClick={() => setTabs((items) => items.map((item) => item.path === selected() ? { ...item, mode: "preview" } : item))}>{t("Preview")}</button>
                    <button aria-pressed={file()?.mode === "code"} onClick={() => setTabs((items) => items.map((item) => item.path === selected() ? { ...item, mode: "code" } : item))}>{t("Code")}</button>
                  </Show>

                  <Button
                    icon="file"
                    label={
                      selection().from !== selection().to
                        ? t("Add selection to context")
                        : t("Add file to context")
                    }
                    hint={shortcutText("Mod+Shift+L", app.state.platform)}
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
                detail={t("Open a file from the explorer to read, edit, or review it.")}
              />
            }
          >
            {(value) => (
              <>
                <Show when={value().pin}>
                  <div class="preview-version-strip">{t("Saved preview version")} <button onClick={() => app.run(restorePreview)}>{t("Restore this version")}</button><button onClick={() => app.run(() => open(value().path, { preview: true }))}>{t("Show current")}</button></div>
                </Show>
                <Show when={value().changed}>
                  <div
                    ref={changedBar}
                    class="file-changed"
                    role="alert"
                    tabindex="-1"
                    onAnimationEnd={(event) => event.currentTarget.classList.remove("attention")}
                  >
                    <Icon name="alert" size="sm" />
                    <span>{t("This file changed on disk while you were editing it.")}</span>
                    <button
                      title={t("Discard your edits and show the file on disk")}
                      onClick={() => app.run(reload)}
                    >
                      {t("Reload")}
                    </button>
                    <button
                      title={t("Keep your edits; saving replaces the file on disk")}
                      onClick={keepMine}
                    >
                      {t("Keep mine")}
                    </button>
                  </div>
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
                                    detail={`${value().mime} · ${t("{size} bytes", { size: value().size })}`}
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
                    line={jump()?.path === value().path ? jump() : undefined}
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
  expanded: (path: string) => boolean;
  toggle: (path: string) => void;
}) {
  const app = useApp();
  const [entries, setEntries] = createStore<FileEntry[]>([]),
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
        // Rows that are still there keep their place, and their open folders.
        setEntries(reconcile(result.entries, { key: "path" }));
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
      <For each={entries}>
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
  expanded: (path: string) => boolean;
  toggle: (path: string) => void;
}) {
  const expanded = () => props.expanded(props.entry.path);
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
            ? props.toggle(props.entry.path)
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
            expanded={props.expanded}
            toggle={props.toggle}
          />
        </div>
      </Show>
    </div>
  );
}
