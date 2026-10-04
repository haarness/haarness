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
import { Icon } from "./ui";
import { floatingBox } from "./native-area";

/** The same project choice in the welcome heading and the new-chat composer. */
export function ProjectPicker(props: { heading?: boolean }) {
  const app = useApp();
  const id = createUniqueId();
  const [open, setOpen] = createSignal(false);
  const [query, setQuery] = createSignal("");
  const [cursor, setCursor] = createSignal("");
  const [position, setPosition] = createSignal({ left: 0, top: 0 });
  const projects = createMemo(() => {
    const values = app.projects();
    return app
      .ordered(
        "list:projects",
        values.map((item) => item.id),
      )
      .map((id) => values.find((item) => item.id === id)!);
  });
  const matches = createMemo(() =>
    projects().filter((item) =>
      `${item.label} ${item.root}`
        .toLocaleLowerCase()
        .includes(query().trim().toLocaleLowerCase()),
    ),
  );
  const selected = app.currentProject;
  const activeId = () => {
    const index = matches().findIndex((item) => item.id === cursor());
    return index < 0 ? undefined : `${id}-${index}`;
  };
  let trigger!: HTMLButtonElement,
    popup!: HTMLDivElement,
    search!: HTMLInputElement,
    list!: HTMLDivElement;
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
    setCursor(selected()?.id ?? projects()[0]?.id ?? "");
    setOpen(true);
  };
  const choose = (action: () => Promise<unknown>) => {
    close(true);
    void app.run(async () => {
      const result = await action();
      if (result && !app.state.selected && app.state.panel === "chat")
        document
          .querySelector<HTMLTextAreaElement>("[data-composer-input]")
          ?.focus();
    });
  };
  createEffect(
    on(open, (visible) => {
      if (!visible) return;
      const place = () => {
        if (!popup) return;
        const anchor = trigger.getBoundingClientRect(),
          box = popup.getBoundingClientRect();
        const above = anchor.top - box.height - 8;
        setPosition({
          left: floatingBox(anchor, box.width).left,
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
        class={props.heading ? "heading-project-picker" : "project-picker"}
        aria-label={
          props.heading ? t("Choose project") : t("Choose project for new chat")
        }
        aria-haspopup="dialog"
        aria-expanded={open()}
        aria-controls={open() ? id : undefined}
        onClick={() => (open() ? close(true) : show())}
        onKeyDown={(event) => {
          if (["ArrowDown", "ArrowUp"].includes(event.key)) {
            event.preventDefault();
            show();
          }
        }}
      >
        <Show when={!props.heading}>
          <Icon name="folder" size="sm" />
        </Show>
        {/* A Cloud session's title is often a sentence; inside the heading's own
            question it must not end in a full stop before the question mark. */}
        {(props.heading
          ? selected()?.label.replace(/[.!?。！？]+$/u, "")
          : selected()?.label) || t("Choose a project")}
        {/* A caret marks this as a control rather than a word in the sentence.
            It contributes no text, so the heading still reads as one line. */}
        <Icon name="down" size="xs" class="picker-caret" />
      </button>
      <Show when={open()}>
        <Portal>
          <div
            ref={popup}
            id={id}
            class="project-popover"
            role="dialog"
            aria-label={t("Choose project")}
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
                aria-label={t("Search projects")}
                placeholder={t("Search projects")}
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
                    const item = matches().find(
                      (item) => item.id === cursor(),
                    );
                    if (item) choose(() => app.selectProject(item.id, true));
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
              ref={list}
              id={id + "-list"}
              role="listbox"
              aria-label={t("Projects")}
              class="project-picker-list"
              style={{
                height:
                  Math.max(1, Math.min(6, projects().length)) * 40 + "px",
              }}
            >
              <For each={matches()}>
                {(item, index) => (
                  <div
                    id={`${id}-${index()}`}
                    role="option"
                    aria-selected={selected()?.id === item.id}
                    class="project-picker-option"
                    classList={{ highlighted: cursor() === item.id }}
                    title={item.root}
                    onPointerMove={() => setCursor(item.id)}
                    onPointerDown={(event) => event.preventDefault()}
                    onClick={() =>
                      choose(() => app.selectProject(item.id, true))
                    }
                  >
                    <Icon name="folder" size="lg" />
                    <span class="project-picker-name">
                      {item.label}
                      <Show
                        when={
                          projects().filter(
                            (value) => value.label === item.label,
                          ).length > 1
                        }
                      >
                        <small>{item.root}</small>
                      </Show>
                    </span>
                    <Show when={selected()?.id === item.id}>
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
                <Icon name="plus" size="lg" />
                {app.currentServer().url ? t("Open project") : t("New project")}
              </button>
            </div>
          </div>
        </Portal>
      </Show>
    </>
  );
}
