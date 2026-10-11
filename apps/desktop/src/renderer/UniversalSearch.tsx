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
import { searchEntries, type SearchEntry } from "./search";
import { chatTitle } from "./chat-title";


export function UniversalSearch(props: { close: () => void }) {
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
    if (!q || !connection) return;
    setLoading(true);
    const timer = setTimeout(() => {
      void window.haarness
        .api<string[]>(connection, "searchFiles", [{ q, limit: 100 }])
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
  const entries = createMemo(() => {
    const q = query().trim();
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
    const threads = (q ? conversations : conversations.slice(0, 8)).map(
      ({ thread, location, key }): SearchEntry => ({
        key: "thread:" + key,
        group: thread.archived ? "Archived chats" : "Chats",
        name: chatTitle(thread.title),
        detail:
          (app.projects().find((item) => item.id === projectKey(location))
            ?.label ?? location.label) +
          (isWorktree(location) ? " · " + t(locationName(location)) : ""),
        keywords: thread.id + " " + location.workspace,
        icon: thread.archived ? "archive" : "chat",
        action: () => app.chooseProjectSession(location.id, thread.id),
      }),
    );
    const connection = app.state.connection?.id;
    const paths = files().map(
      (path): SearchEntry => ({
        key: "file:" + connection + ":" + path,
        group: "Files",
        name: path.split("/").pop() ?? path,
        detail: (app.project()?.label ?? t("Workspace")) + " · " + path,
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
    const configuration = settings().filter((entry) => q || !entry.provider);
    const ranked = searchEntries(
      [...commands, ...configuration, ...projects, ...threads, ...paths].map(
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
    app
      .projects()
      .flatMap((project) => project.locations)
      .some(
        (location) =>
          location.checkout?.exists !== false &&
          app.indexes()[location.id]?.loading,
      );
  function run(entry?: SearchEntry) {
    if (!entry || entry.disabled) return;
    executing = true;
    props.close();
    void app.run(async () => {
      if (
        entry.destination !== "settings" &&
        app.state.settings &&
        !(await app.closeSettings())
      )
        return;
      await entry.action();
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
            run(entries()[index()]);
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
            placeholder={t("Search anything in Haarness…")}
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
          <For each={entries()}>
            {(entry, position) => (
              <>
                <Show
                  when={
                    position() === 0 ||
                    entries()[position() - 1].group !== entry.group
                  }
                >
                  <span class="palette-group">{t(entry.group)}</span>
                </Show>
                <button
                  id={"palette-option-" + position()}
                  role="option"
                  aria-label={entry.name}
                  aria-description={entry.detail}
                  aria-selected={index() === position()}
                  aria-disabled={!!entry.disabled}
                  tabIndex={-1}
                  onPointerMove={() => {
                    setIndex(position());
                    setSelection(entry.key);
                  }}
                  onClick={() => run(entry)}
                >
                  <Show
                    when={entry.provider}
                    fallback={<Icon name={entry.icon} size="sm" />}
                  >
                    <ProviderIcon provider={entry.provider} size="sm" />
                  </Show>
                  <span class="palette-label">
                    <span>{entry.name}</span>
                    <Show when={entry.detail}>
                      <small>{entry.detail}</small>
                    </Show>
                  </span>
                  <Show when={entry.hint}>
                    <kbd dir="ltr">{entry.hint}</kbd>
                  </Show>
                </button>
              </>
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
