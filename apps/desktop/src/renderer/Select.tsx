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
  type JSX,
} from "solid-js";
import { Portal } from "solid-js/web";
import { Icon } from "./icons";
import { dialogFrame, floatingBox, frameTop, shortestList } from "./native-area";

export interface SelectOption {
  value: string | number;
  label: string;
  /** A quiet note after the label in the list, such as "no longer listed". */
  hint?: string;
  group?: string;
  disabled?: boolean;
  icon?: () => JSX.Element;
  triggerIcon?: () => JSX.Element;
  groupIcon?: () => JSX.Element;
}

/** A single themed, accessible picker for composer, settings and form fields. */
export function Select(props: {
  options: SelectOption[];
  value?: string | number;
  defaultValue?: string | number;
  onChange?: (value: string) => void;
  onOpen?: () => void;
  "aria-label": string;
  name?: string;
  class?: string;
  disabled?: boolean;
  searchable?: boolean;
  stayOpenOnSelect?: boolean;
  triggerSuffix?: JSX.Element;
  footer?: (close: () => void) => JSX.Element;
}) {
  const id = createUniqueId();
  const [open, setOpen] = createSignal(false);
  const [local, setLocal] = createSignal(
    String(props.defaultValue ?? props.options[0]?.value ?? ""),
  );
  const [query, setQuery] = createSignal("");
  const [cursor, setCursor] = createSignal("");
  const [position, setPosition] = createSignal({
    left: 0,
    top: 0,
    width: 280,
  });
  // Choices can arrive after mount, as in a dialog that opens before its
  // data has loaded. An uncontrolled control must not stay on a value that
  // is no longer offered.
  createEffect(() => {
    if (props.value !== undefined || !props.options.length) return;
    if (!props.options.some((option) => String(option.value) === local()))
      setLocal(String(props.options[0].value));
  });
  const value = () => String(props.value ?? local());
  const selected = () =>
    props.options.find((option) => String(option.value) === value());
  const searchable = () => props.searchable ?? props.options.length > 8;
  // Every word of the search, in any order: "openrouter haiku" among
  // hundreds of routed models.
  const filtered = createMemo(() => {
    const words = query().toLocaleLowerCase().split(/\s+/).filter(Boolean);
    return props.options.filter((option) => {
      const text = `${option.label} ${option.group ?? ""} ${option.value}`.toLocaleLowerCase();
      return words.every((word) => text.includes(word));
    });
  });
  const enabled = () => filtered().filter((option) => !option.disabled);
  const activeId = () => {
    const index = filtered().findIndex(
      (option) => String(option.value) === cursor(),
    );
    return index < 0 ? undefined : `${id}-${index}`;
  };
  let typed = "",
    typedAt = 0;
  let trigger!: HTMLButtonElement;
  let popup!: HTMLDivElement;
  let list!: HTMLDivElement;
  let search: HTMLInputElement | undefined;
  const close = (focus = false) => {
    setOpen(false);
    if (focus && trigger.isConnected) trigger.focus({ preventScroll: true });
  };
  const choose = (option: SelectOption) => {
    if (option.disabled) return;
    setLocal(String(option.value));
    if (!props.stayOpenOnSelect) close(true);
    props.onChange?.(String(option.value));
    // Selecting a new model may remove a focused footer control.
    if (props.stayOpenOnSelect)
      (search ?? list).focus({ preventScroll: true });
  };
  const reveal = () =>
    queueMicrotask(() => {
      if (!open()) return;
      const item = Array.from(
        list.querySelectorAll<HTMLElement>('[role="option"]'),
      ).find((item) => item.dataset.value === cursor());
      item?.scrollIntoView({ block: "nearest" });
    });
  const show = () => {
    props.onOpen?.();
    typed = "";
    typedAt = 0;
    setQuery("");
    setCursor(
      String(
        (selected()?.disabled ? undefined : selected())?.value ??
          enabled()[0]?.value ??
          "",
      ),
    );
    setOpen(true);
  };
  // The highlight moves only when its row can no longer be chosen. While no
  // row can (a picker busy applying a choice disables them all) it stays put,
  // so the row just chosen keeps it once the rows come back. When it must
  // move, it goes to the selected row, unless a search is under way: a search
  // starts at its first match.
  createEffect(() => {
    if (!open()) return;
    const options = enabled();
    if (!options.length) return;
    if (options.some((option) => String(option.value) === cursor())) return;
    const current = query().trim()
      ? undefined
      : options.find((option) => String(option.value) === value());
    setCursor(String((current ?? options[0]).value));
  });
  const keydown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close(true);
      return;
    }
    if ((event.target as HTMLElement).closest(".select-footer")) return;
    if (event.key === "Tab" && props.footer) return;
    if (event.key === "Tab") {
      close(true);
      return;
    }
    if (
      event.target !== search &&
      event.key.length === 1 &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey &&
      event.key !== " "
    ) {
      event.preventDefault();
      event.stopPropagation();
      typed =
        (Date.now() - typedAt < 600 ? typed : "") +
        event.key.toLocaleLowerCase();
      typedAt = Date.now();
      const option = enabled().find((option) =>
        option.label.toLocaleLowerCase().startsWith(typed),
      );
      if (option) {
        setCursor(String(option.value));
        reveal();
      }
      return;
    }
    if (
      event.key === "Enter" ||
      (event.key === " " && event.target !== search)
    ) {
      event.preventDefault();
      event.stopPropagation();
      const option = enabled().find(
        (option) => String(option.value) === cursor(),
      );
      if (option) choose(option);
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    if (event.target === search && ["Home", "End"].includes(event.key))
      return;
    event.preventDefault();
    event.stopPropagation();
    const options = enabled();
    if (!options.length) return;
    const current = options.findIndex(
      (option) => String(option.value) === cursor(),
    );
    const index =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? options.length - 1
          : (current +
              (event.key === "ArrowDown" ? 1 : -1) +
              options.length) %
            options.length;
    setCursor(String(options[index].value));
    reveal();
  };
  createEffect(
    on(open, (visible) => {
      if (!visible) return;
      const place = () => {
        if (!popup) return;
        const anchor = trigger.getBoundingClientRect();
        // Opened in a dialog (Settings, a form), it stays inside its box.
        const frame = dialogFrame(trigger);
        const preferred = Math.max(anchor.width, searchable() ? 360 : 240);
        const { left, width } = floatingBox(anchor, preferred, Math.min(preferred, 260), frame);
        // Measured at its own height, not one a placement gave it before.
        // The list keeps where it was scrolled to (the chosen option), which
        // the taller box would otherwise reset.
        const scrolled = list.scrollTop;
        popup.style.maxHeight = "";
        const height = popup.getBoundingClientRect().height;
        const inside = frameTop(anchor, height, 6, frame, shortestList(popup, list));
        popup.style.maxHeight = inside?.height ? inside.height + "px" : "";
        list.scrollTop = scrolled;
        setPosition({
          width,
          left,
          top:
            inside?.top ??
            Math.max(
              8,
              anchor.bottom + height + 6 <= window.innerHeight - 8
                ? anchor.bottom + 6
                : anchor.top - height - 6,
            ),
        });
      };
      const outside = (event: Event) => {
        const path = event.composedPath();
        if (path.includes(popup) || path.includes(trigger)) return;
        // A label forwards its click to this button. Let that click toggle the
        // menu once; closing on pointerdown would make it immediately reopen.
        if (
          event.type === "pointerdown" &&
          Array.from(trigger.labels ?? []).some((label) =>
            path.includes(label),
          )
        ) {
          // Native dialogs otherwise focus the dialog itself before forwarding
          // the label click, which would also dismiss via the focus listener.
          event.preventDefault();
          return;
        }
        close();
      };
      const scroll = (event: Event) => {
        if (
          event.target === document ||
          (event.target instanceof Element && event.target.contains(trigger))
        )
          place();
      };
      let observer: ResizeObserver | undefined;
      let disposed = false;
      queueMicrotask(() => {
        if (disposed || !open()) return;
        place();
        (search ?? list).focus({ preventScroll: true });
        reveal();
        observer = new ResizeObserver(place);
        observer.observe(popup);
      });
      // Capture before nested controls or portals stop/retarget the event.
      document.addEventListener("pointerdown", outside, true);
      document.addEventListener("focusin", outside, true);
      document.addEventListener("scroll", scroll, true);
      window.addEventListener("resize", place);
      onCleanup(() => {
        disposed = true;
        observer?.disconnect();
        document.removeEventListener("pointerdown", outside, true);
        document.removeEventListener("focusin", outside, true);
        document.removeEventListener("scroll", scroll, true);
        window.removeEventListener("resize", place);
        search = undefined;
      });
    }),
  );
  return (
    <>
      <Show when={props.name}>
        <input
          type="hidden"
          name={props.name}
          value={value()}
          disabled={props.disabled}
        />
      </Show>
      <button
        ref={trigger}
        type="button"
        role="combobox"
        class={`select-trigger ${props.class ?? ""}`}
        disabled={props.disabled}
        aria-label={props["aria-label"]}
        aria-haspopup="listbox"
        aria-expanded={open()}
        aria-controls={open() ? id : undefined}
        data-value={value()}
        onClick={(event) => {
          event.stopPropagation();
          open() ? close(true) : show();
        }}
        onKeyDown={(event) => {
          if (["ArrowDown", "ArrowUp"].includes(event.key)) {
            event.preventDefault();
            show();
          }
        }}
      >
        {selected()?.triggerIcon?.() ?? selected()?.icon?.()}
        <span class="select-label">{selected()?.label ?? value()}</span>
        {props.triggerSuffix}
        <Icon name="down" size="sm" class="select-caret" />
      </button>
      <Show when={open()}>
        <Portal
          mount={trigger.closest('dialog, [role="dialog"]') ?? document.body}
        >
          <div
            ref={popup}
            class="select-popover"
            classList={{ searchable: searchable() }}
            style={{
              left: position().left + "px",
              top: position().top + "px",
              width: position().width + "px",
            }}
            onKeyDown={keydown}
          >
            <Show when={searchable()}>
              <input
                ref={search}
                class="select-search"
                aria-label={`Search ${props["aria-label"].toLowerCase()}`}
                placeholder={t("Search…")}
                autocomplete="off"
                spellcheck={false}
                value={query()}
                role="combobox"
                aria-expanded="true"
                aria-controls={id}
                aria-activedescendant={activeId()}
                onInput={(event) => {
                  // Filtering must not make the list jump; hold the height
                  // the popover opened with.
                  if (popup && !popup.style.minHeight)
                    popup.style.minHeight = popup.getBoundingClientRect().height + "px";
                  setQuery(event.currentTarget.value);
                  setCursor(String(enabled()[0]?.value ?? ""));
                  reveal();
                }}
              />
            </Show>
            <div
              ref={list}
              id={id}
              class="select-options"
              role="listbox"
              aria-label={props["aria-label"]}
              tabIndex={-1}
              aria-activedescendant={activeId()}
            >
              <For each={filtered()}>
                {(option, index) => (
                  <>
                    <Show
                      when={
                        option.group &&
                        (index() === 0 ||
                          filtered()[index() - 1].group !== option.group)
                      }
                    >
                      <div class="select-group" role="presentation">
                        {option.groupIcon?.()}
                        <span>{option.group}</span>
                      </div>
                    </Show>
                    <div
                      id={`${id}-${index()}`}
                      role="option"
                      aria-selected={value() === String(option.value)}
                      aria-disabled={!!option.disabled}
                      class="select-option"
                      classList={{
                        highlighted: cursor() === String(option.value),
                      }}
                      data-value={String(option.value)}
                      onPointerMove={() =>
                        !option.disabled && setCursor(String(option.value))
                      }
                      onPointerDown={(event) => event.preventDefault()}
                      onClick={(event) => {
                        event.stopPropagation();
                        choose(option);
                      }}
                    >
                      {option.icon?.()}
                      <span class="select-label">{option.label}</span>
                      <Show when={option.hint}>
                        <span class="select-hint">{option.hint}</span>
                      </Show>
                      <Show when={value() === String(option.value)}>
                        <span class="select-check" aria-hidden="true">
                          ✓
                        </span>
                      </Show>
                    </div>
                  </>
                )}
              </For>
              <Show when={!filtered().length}>
                <div class="select-empty">{t("No matches")}</div>
              </Show>
            </div>
            <Show when={props.footer}>
              <div class="select-footer">
                {props.footer?.(() => close(true))}
              </div>
            </Show>
          </div>
        </Portal>
      </Show>
    </>
  );
}
