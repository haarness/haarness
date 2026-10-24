import { createSettingsSearch } from "./settings-search";
import { t } from "./i18n";
import { ProviderIcon } from "./ProviderIcon";
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from "solid-js";
import { useApp } from "./state";
import { dismissOnBackdrop, Icon } from "./ui";
import { isWorktree, locationName, primaryLocation, projectKey } from "../projects";
import { asksForKeys, searchEntries, type SearchEntry } from "./search";
import { chatTitle } from "./chat-title";
import { focusComposerWhenShown } from "./composer-focus";
import { folderName } from "./chat-folder";


/** Where Search lists archived chats: a project's, one checkout's of it, or
 * every project's. */
export type ArchivedScope = { project?: string; location?: string };
/** Search opened on archived chats (the sidebar's "N archived" under a list
 * with no open chats). */
const [archivedRequest, setArchivedRequest] = createSignal<ArchivedScope & { at: number }>();
export const archivedSearchRequested = archivedRequest;
export const searchArchived = (scope: ArchivedScope) => setArchivedRequest({ ...scope, at: Date.now() });

export function UniversalSearch(props: {
  close: () => void;
  /** Only archived chats, of this project or checkout when it names one. */
  archived?: ArchivedScope;
}) {
  const app = useApp();
  const settings = createSettingsSearch(app, { inventory: false });
  let dialog!: HTMLDialogElement;
  onMount(() => dialog.showModal());
  onCleanup(() => dialog.close());
  const [query, setQuery] = createSignal("");
  const [index, setIndex] = createSignal(0);
  const [selection, setSelection] = createSignal("");
  const [files, setFiles] = createSignal<string[]>([]);
  const [loading, setLoading] = createSignal(false);
  const [problem, setProblem] = createSignal("");
  const previous = document.activeElement as HTMLElement | null;
  let executing = false;
  app.set("searchOpen", true);
  onCleanup(() => {
    app.set("searchOpen", false);
    if (!executing)
      queueMicrotask(
        () =>
          previous?.isConnected && previous.focus({ preventScroll: true }),
      );
  });
  createEffect(() => {
    const q = query().trim();
    const connection = app.state.connection?.id;
    let disposed = false;
    setFiles([]);
    setProblem("");
    setLoading(false);
    // A chat of This computer before its first message has no files yet.
    if (!q || !connection || props.archived || app.unfoldered()) return;
    setLoading(true);
    const timer = setTimeout(() => {
      void window.haarness
        .api<string[]>(connection, "searchFiles", [{ q, limit: 100, ...app.fileScope() }])
        .then((values) => {
          if (!disposed) setFiles(values);
        })
        .catch(() => {
          if (!disposed)
            setProblem(t("File search is unavailable in this workspace."));
        })
        .finally(() => {
          if (!disposed) setLoading(false);
        });
    }, 150);
    onCleanup(() => {
      disposed = true;
      clearTimeout(timer);
    });
  });
  type Conversation = ReturnType<typeof app.conversations>[number];
  const chatEntry = ({ thread, location, key }: Conversation): SearchEntry => ({
    key: "thread:" + key,
    group: thread.archived ? "Archived chats" : "Chats",
    name: chatTitle(thread.title),
    // A chat without a project is This computer's, in a folder of its own.
    detail: location.chatHome
      ? t("This computer") +
        (thread.directory ? " · " + folderName(thread.directory) : "")
      : (app.projects().find((item) => item.id === projectKey(location))
          ?.label ?? location.label) +
        (isWorktree(location) ? " · " + t(locationName(location)) : ""),
    keywords:
      thread.id +
      " " +
      (thread.directory ?? location.workspace) +
      (location.chatHome ? " " + t("This computer") : ""),
    icon: thread.archived ? "archive" : "chat",
    action: (open) =>
      app.chooseProjectSession(location.id, thread.id, open?.keep ? "keep" : "preview"),
  });
  const entries = createMemo(() => {
    const q = query().trim();
    const scope = props.archived;
    if (scope)
      return searchEntries(
        app
          .conversations(scope.project)
          .filter(({ thread, location }) =>
            thread.archived &&
            !thread.isSubagent &&
            location.checkout?.exists !== false &&
            (!scope.location || location.id === scope.location),
          )
          .sort((a, b) => b.thread.updatedAt.localeCompare(a.thread.updatedAt))
          .map(chatEntry),
        q,
      );
    const commands = app.searchCommands();
    const projects = app.projects().flatMap((project): SearchEntry[] => {
      const root = primaryLocation(project);
      return [
        ...(root.checkout?.exists === false
          ? []
          : [
              {
                key: "project:" + project.id,
                group: "Projects",
                name: project.label,
                detail: project.root,
                icon: "folder" as const,
                keywords: "project workspace open",
                action: () => app.activateProject(root.id),
              },
            ]),
        ...project.locations
          .filter(
            (location) =>
              isWorktree(location) && location.checkout?.exists !== false,
          )
          .map(
            (location): SearchEntry => ({
              key: "worktree:" + location.id,
              group: "Worktrees",
              name: t(locationName(location)),
              detail: project.label + " · " + location.workspace,
              keywords: location.checkout?.branch ?? "",
              icon: "branch",
              action: () => app.activateProject(location.id),
            }),
          ),
      ];
    });
    const conversations = app
      .conversations()
      .filter(
        ({ thread, location }) =>
          !thread.isSubagent &&
          location.checkout?.exists !== false &&
          (q || !thread.archived),
      )
      .sort((a, b) => b.thread.updatedAt.localeCompare(a.thread.updatedAt));
    const threads = (q ? conversations : conversations.slice(0, 8)).map(chatEntry);
    const connection = app.state.connection?.id;
    const paths = files().map(
      (path): SearchEntry => ({
        key: "file:" + connection + ":" + path,
        group: "Files",
        name: path.split("/").pop() ?? path,
        detail:
          (app.project()?.chatHome
            ? t("This computer")
            : (app.project()?.label ?? t("Workspace"))) +
          " · " +
          path,
        icon: "file",
        action: () => {
          if (app.state.connection?.id !== connection) return;
          app.set("fileRequest", {
            connection: connection!,
            path,
            sequence: Date.now(),
          });
          app.set("panel", "files");
        },
      }),
    );
    // Providers are a long, brand-heavy list. Keep them findable by name, but
    // out of the default list — the same rule file results already follow.
    // A "Shortcut: …" entry repeats the action it belongs to, whose row shows
    // the key anyway, so it comes only when the search is about keys.
    const aboutKeys = asksForKeys(q, [t("Shortcuts"), t("Keyboard shortcuts")]);
    const configuration = settings().filter(
      (entry) => (q || !entry.provider) && (aboutKeys || entry.group !== "Shortcuts"),
    );
    // With nothing typed, the list leads with the two ways to start.
    const leading = q ? [] : ["new", "open"];
    const ordered = [
      ...leading.flatMap((key) => commands.filter((entry) => entry.key === key)),
      ...commands.filter((entry) => !leading.includes(entry.key)),
    ];
    const ranked = searchEntries(
      [...ordered, ...configuration, ...projects, ...threads, ...paths].map(
        (entry) => ({
          ...entry,
          keywords: [entry.keywords, t(entry.group)].filter(Boolean).join(" "),
        }),
      ),
      q,
    );
    // Keep chats ahead of actions, with relevance ordering within each group.
    // The remaining categories follow their strongest match.
    const groups = new Map<string, SearchEntry[]>();
    for (const entry of ranked)
      groups.set(entry.group, [...(groups.get(entry.group) ?? []), entry]);
    const chats = ["Chats", "Archived chats"].flatMap((group) => {
      const matches = groups.get(group) ?? [];
      groups.delete(group);
      return matches;
    });
    return [...chats, ...[...groups.values()].flat()];
  });
  // Rows follow their result's key rather than its object: the list is rebuilt
  // whenever any chat, turn or setting changes, and a row rebuilt under the
  // pointer would lose the click that was landing on it.
  const keys = createMemo(() => entries().map((entry) => entry.key));
  const byKey = createMemo(
    () => new Map(entries().map((entry) => [entry.key, entry])),
  );
  createEffect(() => {
    const size = entries().length;
    const selected = entries().findIndex(
      (entry) => entry.key === selection(),
    );
    setIndex((value) =>
      selected >= 0 ? selected : Math.min(value, Math.max(0, size - 1)),
    );
  });
  const indexing = () =>
    [...app.projects(), ...(app.chatHomeGroup() ? [app.chatHomeGroup()!] : [])]
      .flatMap((project) => project.locations)
      .some(
        (location) =>
          location.checkout?.exists !== false &&
          app.indexes()[location.id]?.loading,
      );
  /** The project, and the checkout when it is a worktree, Search is limited to. */
  const scopeName = () => {
    const project =
      app.projects().find((item) => item.id === props.archived?.project) ??
      (app.chatHomeGroup()?.id === props.archived?.project ? app.chatHomeGroup() : undefined);
    const location = project?.locations.find((item) => item.id === props.archived?.location);
    return (project?.label ?? "") + (isWorktree(location) ? " · " + t(locationName(location!)) : "");
  };
  /** With Command (Control elsewhere) held, a chat opens in a tab that stays. */
  const keeps = (event: MouseEvent | KeyboardEvent) =>
    app.state.platform === "darwin" ? event.metaKey : event.ctrlKey;
  function run(entry?: SearchEntry, open: { keep?: boolean } = {}) {
    if (!entry || entry.disabled) return;
    executing = true;
    props.close();
    // Closing the dialog gives the keyboard back to what had it, often the
    // sidebar's Search button. A chat or project leads to its message box;
    // any other pick that leaves the keyboard on that button (or nowhere)
    // sends it to the message box too, while one that took the keyboard
    // somewhere (a dialog, the terminal) keeps it there.
    const leads = /^(thread|project|worktree):/.test(entry.key);
    const opener = previous?.matches("[data-search-button]") ? previous : null;
    void app.run(async () => {
      if (
        entry.destination !== "settings" &&
        app.state.settings &&
        !(await app.closeSettings())
      )
        return;
      await entry.action(open);
      if (leads || opener) focusComposerWhenShown(leads ? previous : opener);
    });
  }
  return (
      <dialog
        ref={dialog}
        class="palette palette-dialog"
        {...dismissOnBackdrop(props.close)}
        onCancel={(event) => { event.preventDefault(); props.close(); }}
        aria-label={t("Universal search")}
        aria-modal="true"
        onKeyDown={(event) => {
          if (event.isComposing) return;
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            props.close();
            return;
          }
          if (
            ["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key) &&
            !(event.ctrlKey || event.metaKey)
          ) {
            event.preventDefault();
            const size = entries().length;
            setIndex((value) =>
              event.key === "Home"
                ? 0
                : event.key === "End"
                  ? Math.max(0, size - 1)
                  : (value + (event.key === "ArrowDown" ? 1 : -1) + size) %
                    Math.max(size, 1),
            );
            document
              .getElementById("palette-option-" + index())
              ?.scrollIntoView({ block: "nearest" });
            setSelection(entries()[index()]?.key ?? "");
          }
          if (event.key === "Enter") {
            event.preventDefault();
            run(entries()[index()], { keep: keeps(event) });
          }
        }}
      >
        <div class="palette-field">
          <Icon name="search" size="md" />
          <input
            aria-label={t("Search Haarness")}
            role="combobox"
            aria-autocomplete="list"
            aria-expanded="true"
            aria-controls="universal-search-results"
            aria-activedescendant={
              entries().length ? "palette-option-" + index() : undefined
            }
            ref={(element) => queueMicrotask(() => element.focus())}
            placeholder={
              !props.archived
                ? t("Search anything in Haarness…")
                : props.archived.project
                  ? t("Search archived chats in {name}…", { name: scopeName() })
                  : t("Search archived chats…")
            }
            value={query()}
            onInput={(event) => {
              setQuery(event.currentTarget.value);
              setSelection("");
              setIndex(0);
            }}
          />
          <kbd>{t("ESC")}</kbd>
        </div>
        <div
          class="palette-results"
          role="listbox"
          id="universal-search-results"
          aria-label={t("Search results")}
        >
          <For each={keys()}>
            {(key, position) => (
              <Show when={byKey().get(key)}>
                {(entry) => (
                  <>
                    <Show
                      when={
                        position() === 0 ||
                        entries()[position() - 1]?.group !== entry().group
                      }
                    >
                      <span class="palette-group">{t(entry().group)}</span>
                    </Show>
                    <button
                      id={"palette-option-" + position()}
                      role="option"
                      aria-label={entry().name}
                      aria-description={entry().detail}
                      aria-selected={index() === position()}
                      aria-disabled={!!entry().disabled}
                      tabIndex={-1}
                      onPointerMove={() => {
                        setIndex(position());
                        setSelection(key);
                      }}
                      onClick={(event) => run(entry(), { keep: keeps(event) })}
                      onMouseDown={(event) => {
                        if (event.button === 1) event.preventDefault();
                      }}
                      onAuxClick={(event) => {
                        if (event.button === 1) run(entry(), { keep: true });
                      }}
                    >
                      <Show
                        when={entry().provider}
                        fallback={<Icon name={entry().icon} size="sm" />}
                      >
                        <ProviderIcon provider={entry().provider} size="sm" />
                      </Show>
                      <span class="palette-label">
                        <span>{entry().name}</span>
                        <Show when={entry().detail}>
                          <small>{entry().detail}</small>
                        </Show>
                      </span>
                      <Show when={entry().hint}>
                        <kbd dir="ltr">{entry().hint}</kbd>
                      </Show>
                    </button>
                  </>
                )}
              </Show>
            )}
          </For>
          <Show when={!entries().length}>
            <p class="palette-empty">
              {t("Nothing matches “{query}”. Try a command, setting, project, chat, or file name.", { query: query() })}
            </p>
          </Show>
        </div>
        <div class="palette-footer" role="status">
          <span class="palette-count">
            {loading() || indexing()
              ? t("Searching…")
              : problem() ||
                t(entries().length === 1 ? "{count} result" : "{count} results", {
                  count: entries().length,
                })}
          </span>
          {/* Keys stay Latin-ordered so the arrows do not migrate in RTL. */}
          <span class="palette-hint">
            <kbd dir="ltr">↑</kbd>
            <kbd dir="ltr">↓</kbd>
            {t("to navigate")}
            <span aria-hidden="true">·</span>
            <kbd dir="ltr">↵</kbd>
            {t("to open")}
          </span>
        </div>
      </dialog>
  );
}
