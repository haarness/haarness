import { t } from "./i18n";
import { createSignal, onMount, onCleanup, For, Show } from "solid-js";
import { FormDialog } from "./FormDialog";
import { Button, Icon } from "./ui";
import { useApp } from "./state";
import { remoteProjectError } from "./remote-project-error";
import type { DirectoryListing, SavedServer } from "../shared";
import "./remote-project-picker.css";

type Folder = DirectoryListing["directories"][number];
export function RemoteProjectPicker(props: {
  server: SavedServer;
  newChat: boolean;
  close: () => void;
}) {
  const app = useApp();
  const [path, setPath] = createSignal("");
  const [listing, setListing] = createSignal<DirectoryListing>();
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal("");
  const [history, setHistory] = createSignal<string[]>([]);
  const [suggestions, setSuggestions] = createSignal<Folder[]>([]);
  const [selected, setSelected] = createSignal(-1);
  let input!: HTMLInputElement;
  let browser!: HTMLDivElement;
  let generation = 0;
  let completion = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const dismissSuggestions = () => {
    completion++;
    clearTimeout(timer);
    setSuggestions([]);
    setSelected(-1);
  };
  onCleanup(() => {
    generation++;
    dismissSuggestions();
  });
  async function browse(
    value?: string,
    mode: "push" | "back" | "more" | "refresh" = "push",
  ) {
    dismissSuggestions();
    const attempt = ++generation;
    const restoreFocus = !!document.activeElement?.closest(
      ".remote-folder-list",
    );
    if (mode !== "more" && browser) browser.scrollTop = 0;
    setLoading(true);
    setError("");
    const previous = listing();
    try {
      const { listing: result } = await window.haarness.remoteProjects(
        props.server.id,
        value,
        {
          hidden: true,
          limit: 200,
          after:
            mode === "more" ? (previous?.nextCursor ?? undefined) : undefined,
        },
      );
      if (attempt !== generation) return;
      if (mode === "back") setHistory((items) => items.slice(0, -1));
      else if (mode === "push" && previous && previous.path !== result.path)
        setHistory((items) => [...items, previous.path]);
      setListing(
        mode === "more" && previous
          ? {
              ...result,
              directories: [...previous.directories, ...result.directories],
            }
          : result,
      );
      setPath(result.path);
      queueMicrotask(() => {
        if (attempt !== generation) return;
        if (mode !== "more") browser.scrollTop = 0;
        if (restoreFocus)
          (
            browser.querySelector<HTMLButtonElement>(
              ".remote-folder-list button:not(:disabled)",
            ) ?? input
          ).focus();
      });
    } catch {
      if (attempt === generation)
        setError(
          t(
            "Couldn’t read this folder. Check the path and your access to the server.",
          ),
        );
    } finally {
      if (attempt === generation) setLoading(false);
    }
  }
  function edit(value: string) {
    generation++;
    setLoading(false);
    dismissSuggestions();
    setPath(value);
    setError("");
    const slash = Math.max(value.lastIndexOf("/"), value.lastIndexOf("\\"));
    if (slash < 0) return;
    const parent = value.slice(0, slash + 1);
    const prefix = value.slice(slash + 1);
    const attempt = completion;
    timer = setTimeout(async () => {
      try {
        const { listing: result } = await window.haarness.remoteProjects(
          props.server.id,
          parent,
          { prefix, hidden: true, limit: 20 },
        );
        if (attempt !== completion) return;
        // Filtering here also supports hosts predating prefix queries.
        setSuggestions(
          result.directories
            .filter((folder) =>
              folder.name.toLowerCase().startsWith(prefix.toLowerCase()),
            )
            .slice(0, 20),
        );
      } catch {
        if (attempt === completion) setSuggestions([]);
      }
    }, 180);
  }
  onMount(() => void browse());
  return (
    <FormDialog
      class="remote-project-picker"
      title={t("Open remote project")}
      description={t("Choose a folder on {server}.", {
        server: props.server.label,
      })}
      submit={t("Open project")}
      pendingLabel={t("Opening…")}
      submitDisabled={!path().trim() || loading()}
      close={props.close}
      save={async () => {
        dismissSuggestions();
        generation++;
        if (app.state.activeServer !== props.server.id)
          throw new Error(
            t("The selected server changed. Reopen the folder explorer."),
          );
        try {
          if (
            !(await app.connect(
              { url: props.server.url, workspace: path().trim() },
              props.newChat,
              true,
            ))
          )
            throw new Error("Cancelled");
        } catch (error) {
          throw new Error(remoteProjectError(error));
        }
      }}
    >
      {(busy) => (
        <>
          <div class="remote-folder-toolbar">
            <Button
              icon="chevron"
              class="remote-folder-back"
              label={t("Back")}
              disabled={!history().length || busy()}
              onClick={() => void browse(history().at(-1), "back")}
            />
            <Button
              icon="up"
              label={t("Up one folder")}
              disabled={!listing()?.parent || busy()}
              onClick={() => void browse(listing()!.parent!)}
            />
            <button
              type="button"
              disabled={busy()}
              onClick={() => void browse("~")}
            >
              {t("Home")}
            </button>
            <div class="remote-folder-address">
              <input
                ref={input}
                aria-label={t("Remote folder")}
                role="combobox"
                aria-autocomplete="list"
                aria-expanded={suggestions().length > 0}
                aria-controls="remote-path-suggestions"
                aria-activedescendant={
                  selected() >= 0
                    ? `remote-path-option-${selected()}`
                    : undefined
                }
                autocomplete="off"
                spellcheck={false}
                value={path()}
                placeholder={t("Enter a folder path")}
                onInput={(event) => edit(event.currentTarget.value)}
                onBlur={dismissSuggestions}
                onKeyDown={(event) => {
                  const choices = suggestions();
                  if (event.key === "Escape" && choices.length) {
                    event.preventDefault();
                    event.stopPropagation();
                    dismissSuggestions();
                  } else if (
                    (event.key === "ArrowDown" || event.key === "ArrowUp") &&
                    choices.length
                  ) {
                    event.preventDefault();
                    setSelected((index) =>
                      event.key === "ArrowDown"
                        ? (index + 1) % choices.length
                        : index <= 0
                          ? choices.length - 1
                          : index - 1,
                    );
                    document
                      .getElementById(`remote-path-option-${selected()}`)
                      ?.scrollIntoView({ block: "nearest" });
                  } else if (event.key === "Tab" && choices.length) {
                    event.preventDefault();
                    const folder = choices[Math.max(0, selected())];
                    edit(
                      folder.path + (folder.path.includes("\\") ? "\\" : "/"),
                    );
                  } else if (event.key === "Enter") {
                    event.preventDefault();
                    void browse(
                      selected() >= 0 ? choices[selected()]?.path : path(),
                    );
                  }
                }}
              />
              <Show when={suggestions().length}>
                <ul
                  id="remote-path-suggestions"
                  role="listbox"
                  aria-label={t("Suggested folders")}
                  class="remote-path-suggestions"
                >
                  <For each={suggestions()}>
                    {(folder, index) => (
                      <li
                        id={`remote-path-option-${index()}`}
                        role="option"
                        aria-selected={selected() === index()}
                        onPointerDown={(event) => event.preventDefault()}
                        onClick={() => {
                          void browse(folder.path);
                          input.focus();
                        }}
                      >
                        <Icon name="folder" />
                        <span>{folder.name}</span>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
            </div>
          </div>
          <div class="remote-folder-heading">
            <span title={listing()?.path}>
              {listing()?.path ?? t("Folders")}
            </span>
          </div>
          <div
            ref={browser}
            class="remote-folder-browser"
            aria-busy={loading()}
          >
            <Show when={loading()}>
              <p role="status" class="remote-folder-status">
                {t("Loading folders…")}
              </p>
            </Show>
            <Show when={error()}>
              <div class="remote-folder-status" role="alert">
                <p>{error()}</p>
                <button
                  type="button"
                  onClick={() => void browse(path(), "refresh")}
                >
                  {t("Try again")}
                </button>
              </div>
            </Show>
            <Show when={!error() && listing()}>
              <ul
                aria-label={t("Remote folders")}
                class="remote-folder-list"
                onKeyDown={(event) => {
                  if (
                    !["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)
                  )
                    return;
                  const buttons = [
                    ...event.currentTarget.querySelectorAll<HTMLButtonElement>(
                      "button",
                    ),
                  ];
                  const index = buttons.indexOf(
                    document.activeElement as HTMLButtonElement,
                  );
                  if (index < 0) return;
                  event.preventDefault();
                  buttons[
                    event.key === "Home"
                      ? 0
                      : event.key === "End"
                        ? buttons.length - 1
                        : Math.max(
                            0,
                            Math.min(
                              buttons.length - 1,
                              index + (event.key === "ArrowDown" ? 1 : -1),
                            ),
                          )
                  ]?.focus();
                }}
              >
                <For each={listing()?.directories}>
                  {(folder) => (
                    <li>
                      <button
                        type="button"
                        disabled={loading()}
                        onClick={() => void browse(folder.path)}
                        title={folder.name}
                      >
                        <Icon name="folder" />
                        <span>{folder.name}</span>
                        <Icon name="chevron" />
                      </button>
                    </li>
                  )}
                </For>
              </ul>
              <Show when={!loading() && !listing()?.directories.length}>
                <p class="remote-folder-status">
                  {t("No subfolders. You can open this folder as a project.")}
                </p>
              </Show>
              <Show when={listing()?.nextCursor}>
                <button
                  type="button"
                  class="remote-folder-more"
                  disabled={loading()}
                  onClick={() => void browse(listing()?.path, "more")}
                >
                  {t("Show more folders")}
                </button>
              </Show>
            </Show>
          </div>
        </>
      )}
    </FormDialog>
  );
}
