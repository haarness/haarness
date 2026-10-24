import { t } from "./i18n";
import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  For,
  on,
  onCleanup,
  Show,
} from "solid-js";
import { Portal } from "solid-js/web";
import { useApp } from "./state";
import { Icon, type IconName } from "./ui";
import { MenuButton } from "./MenuButton";
import { floatingBox } from "./native-area";
import { localServer } from "../servers";
import { folderName } from "./chat-folder";
import { shortPath } from "./transcript";

/** The No folder choice's place in the list, beside the projects' IDs. */
const NO_FOLDER = "\u0000no-folder";

/** The words for showing a folder in the system file manager. */
export const revealLabel = (platform: string) =>
  platform === "darwin"
    ? t("Reveal in Finder")
    : platform === "win32"
      ? t("Show in Explorer")
      : t("Show in file manager");

/**
 * Where the message box's chat works, as a chip in its bottom row. A new
 * chat chooses: No folder (on this computer: a folder of its own in the chat
 * home), one of the projects, or another folder. A chat that has started
 * keeps where it works; its chip names that folder and shows it in the file
 * manager.
 */
export function FolderPicker() {
  const app = useApp();
  return (
    <Show when={app.state.selected} fallback={<NewChatFolder />}>
      <ChatFolder />
    </Show>
  );
}

function ChatFolder() {
  const app = useApp();
  const { state } = app;
  const directory = app.chatFolder;
  /** The folder the chat works in, as a path of this computer or its server. */
  const path = () =>
    state.connection?.chatHome ? directory() : state.connection?.workspace;
  const name = () =>
    state.connection?.chatHome
      ? directory()
        ? folderName(directory()!)
        : t("No folder")
      : (app.currentProject()?.label ?? app.project()?.label ?? "");
  return (
    <Show
      when={!state.connection?.remote && path()}
      fallback={
        <span class="composer-folder static" title={path()}>
          <Icon name="folder" size="sm" />
          <span class="picker-label">{name()}</span>
        </span>
      }
    >
      <MenuButton
        class="composer-folder"
        label={t("Chat folder: {name}", { name: name() })}
        title={path()}
        content={
          <>
            <Icon name="folder" size="sm" />
            <span class="picker-label">{name()}</span>
          </>
        }
        items={[
          {
            label: revealLabel(state.platform),
            detail: shortPath(path() ?? "", 3),
            icon: "folder-open",
            run: () =>
              void app.run(() =>
                window.haarness.reveal(
                  state.connection!.id,
                  state.connection!.chatHome ? path() : undefined,
                ),
              ),
          },
        ]}
      />
    </Show>
  );
}

function NewChatFolder() {
  const app = useApp();
  const { state } = app;
  const id = createUniqueId();
  const [open, setOpen] = createSignal(false);
  const [query, setQuery] = createSignal("");
  const [cursor, setCursor] = createSignal("");
  const [position, setPosition] = createSignal({ left: 0, top: 0 });
  /** Chats need no project on this computer only. */
  const local = () => app.currentServer().id === localServer.id;
  const folderless = () => !!app.project()?.chatHome;
  const projects = createMemo(() => {
    const values = app.projects();
    return app
      .ordered(
        "list:projects",
        values.map((item) => item.id),
      )
      .map((id) => values.find((item) => item.id === id)!);
  });
  type Choice = { id: string; label: string; detail?: string; icon: IconName };
  const choices = createMemo<Choice[]>(() => [
    ...(local()
      ? [{ id: NO_FOLDER, label: t("No folder"), icon: "chat" as const }]
      : []),
    ...projects().map((item) => ({
      id: item.id,
      label: item.label,
      // Two projects of one name are told apart by where they are.
      detail:
        projects().filter((value) => value.label === item.label).length > 1
          ? item.root
          : undefined,
      icon: "folder" as const,
    })),
  ]);
  const matches = createMemo(() => {
    const words = query().trim().toLocaleLowerCase();
    return choices().filter((item) =>
      `${item.label} ${item.id === NO_FOLDER ? "" : (projects().find((project) => project.id === item.id)?.root ?? "")}`
        .toLocaleLowerCase()
        .includes(words),
    );
  });
  const chosen = () =>
    folderless() ? NO_FOLDER : (app.currentProject()?.id ?? "");
  const label = () =>
    folderless()
      ? t("No folder")
      : (app.currentProject()?.label ?? t("Choose a folder"));
  const activeId = () => {
    const index = matches().findIndex((item) => item.id === cursor());
    return index < 0 ? undefined : `${id}-${index}`;
  };
  let trigger!: HTMLButtonElement,
    popup!: HTMLDivElement,
    search!: HTMLInputElement;
  const close = (focus = false) => {
    setOpen(false);
    if (focus && trigger.isConnected) trigger.focus({ preventScroll: true });
  };
  const reveal = () =>
    queueMicrotask(() => {
      if (open())
        document
          .getElementById(activeId() ?? "")
          ?.scrollIntoView({ block: "nearest" });
    });
  const show = () => {
    setQuery("");
    setCursor(chosen() || (choices()[0]?.id ?? ""));
    setOpen(true);
  };
  /** Runs a choice, and gives the keyboard to the message box once the new
   * chat is there. */
  const choose = (action: () => Promise<unknown>) => {
    close(true);
    void app.run(async () => {
      const result = await action();
      if (result !== false && !state.selected && state.panel === "chat")
        document
          .querySelector<HTMLTextAreaElement>("[data-composer-input]")
          ?.focus();
    });
  };
  const pick = (choice: Choice) => {
    if (choice.id === chosen()) return close(true);
    choose(() =>
      choice.id === NO_FOLDER
        ? app.startFolderlessChat()
        : app.selectProject(choice.id, true),
    );
  };
  createEffect(
    on(open, (visible) => {
      if (!visible) return;
      const place = () => {
        if (!popup) return;
        const anchor = trigger.getBoundingClientRect(),
          box = popup.getBoundingClientRect();
        const above = anchor.top - box.height - 8;
        // It opens from the chip's start: its right edge in a right-to-left window.
        const rtl = getComputedStyle(trigger).direction === "rtl";
        setPosition({
          left: rtl
            ? Math.max(8, anchor.right - box.width)
            : floatingBox(anchor, box.width).left,
          top: Math.max(
            8,
            above >= 8
              ? above
              : Math.min(
                  anchor.bottom + 8,
                  window.innerHeight - box.height - 8,
                ),
          ),
        });
      };
      const outside = (event: Event) => {
        if (
          !popup?.contains(event.target as Node) &&
          !trigger.contains(event.target as Node)
        )
          close();
      };
      const scroll = (event: Event) => {
        if (
          event.target === document ||
          (event.target instanceof Element && event.target.contains(trigger))
        )
          place();
      };
      const observer = new ResizeObserver(place);
      queueMicrotask(() => {
        if (!open()) return;
        place();
        observer.observe(popup);
        search.focus({ preventScroll: true });
        reveal();
      });
      document.addEventListener("pointerdown", outside);
      document.addEventListener("focusin", outside);
      document.addEventListener("scroll", scroll, true);
      window.addEventListener("resize", place);
      onCleanup(() => {
        observer.disconnect();
        document.removeEventListener("pointerdown", outside);
        document.removeEventListener("focusin", outside);
        document.removeEventListener("scroll", scroll, true);
        window.removeEventListener("resize", place);
      });
    }),
  );
  return (
    <>
      <button
        ref={trigger}
        type="button"
        class="composer-folder"
        aria-label={t("Folder for new chat: {name}", { name: label() })}
        aria-haspopup="dialog"
        aria-expanded={open()}
        aria-controls={open() ? id : undefined}
        title={folderless() ? undefined : app.project()?.workspace}
        disabled={!!state.openingProject && !app.project()}
        onClick={() => (open() ? close(true) : show())}
        onKeyDown={(event) => {
          if (["ArrowDown", "ArrowUp"].includes(event.key)) {
            event.preventDefault();
            show();
          }
        }}
      >
        <Icon name="folder" size="sm" />
        {/* Its own box, so a long name gives way with an ellipsis and the
            caret stays. */}
        <span class="picker-label">{label()}</span>
        <Icon name="down" size="xs" class="picker-caret" />
      </button>
      <Show when={open()}>
        <Portal>
          <div
            ref={popup}
            id={id}
            class="project-popover"
            role="dialog"
            aria-label={t("Choose a folder")}
            style={{
              left: position().left + "px",
              top: position().top + "px",
            }}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                close(true);
              }
              if (
                event.key === "Tab" &&
                ((event.shiftKey && event.target === search) ||
                  (!event.shiftKey &&
                    (event.target as HTMLElement).dataset.lastAction))
              ) {
                event.preventDefault();
                close(true);
              }
            }}
          >
            <div class="project-picker-search">
              <Icon name="search" size="md" />
              <input
                ref={search}
                role="combobox"
                aria-label={t("Search folders")}
                placeholder={t("Search folders")}
                autocomplete="off"
                spellcheck={false}
                aria-expanded="true"
                aria-autocomplete="list"
                aria-controls={id + "-list"}
                aria-activedescendant={activeId()}
                value={query()}
                onInput={(event) => {
                  setQuery(event.currentTarget.value);
                  setCursor(matches()[0]?.id ?? "");
                  reveal();
                }}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    const item = matches().find((item) => item.id === cursor());
                    if (item) pick(item);
                  }
                  if (!["ArrowDown", "ArrowUp"].includes(event.key)) return;
                  event.preventDefault();
                  const values = matches();
                  if (!values.length) return;
                  const current = values.findIndex(
                    (item) => item.id === cursor(),
                  );
                  setCursor(
                    values[
                      (current +
                        (event.key === "ArrowDown" ? 1 : -1) +
                        values.length) %
                        values.length
                    ].id,
                  );
                  reveal();
                }}
              />
            </div>
            <div
              id={id + "-list"}
              role="listbox"
              aria-label={t("Folders")}
              class="project-picker-list"
              style={{
                height:
                  Math.max(1, Math.min(6, choices().length)) * 40 + "px",
              }}
            >
              <For each={matches()}>
                {(item, index) => (
                  <div
                    id={`${id}-${index()}`}
                    role="option"
                    aria-selected={chosen() === item.id}
                    class="project-picker-option"
                    classList={{
                      highlighted: cursor() === item.id,
                      "no-folder": item.id === NO_FOLDER,
                    }}
                    title={item.detail}
                    onPointerMove={() => setCursor(item.id)}
                    onPointerDown={(event) => event.preventDefault()}
                    onClick={() => pick(item)}
                  >
                    <Icon name={item.icon} size="lg" />
                    <span class="project-picker-name">
                      {item.label}
                      <Show when={item.detail}>
                        <small>{item.detail}</small>
                      </Show>
                    </span>
                    <Show when={chosen() === item.id}>
                      <Icon name="check" size="lg" />
                    </Show>
                  </div>
                )}
              </For>
              <Show when={!matches().length}>
                <div class="project-picker-empty" role="status">
                  {query() ? t("No projects found") : t("No projects yet")}
                </div>
              </Show>
            </div>
            <div class="project-picker-actions">
              <button
                type="button"
                data-last-action="true"
                onClick={() => choose(() => app.openProject(true))}
              >
                <Icon name="folder-open" size="lg" />
                {local()
                  ? t("Open folder…")
                  : app.currentServer().url
                    ? t("Open project")
                    : t("New project")}
              </button>
            </div>
          </div>
        </Portal>
      </Show>
    </>
  );
}
