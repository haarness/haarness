import { t } from "./i18n";
import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  on,
  onCleanup,
} from "solid-js";
import { Portal } from "solid-js/web";
import { Icon } from "./icons";
import { floatingBox } from "./native-area";

export interface ComboboxOption {
  value: string;
  /** A few words on the value, already translated. */
  label?: string;
}

/**
 * A text field with suggestions. Any text is accepted; the suggestions only
 * help. They appear while typing, or with ↓ or the caret, filtered by what
 * is being typed; ↑/↓ move, Enter or a click puts one in, and Esc closes
 * them without closing the dialog around the field. Enter with nothing
 * highlighted belongs to the form.
 */
export function Combobox(props: {
  value: string;
  onInput: (value: string) => void;
  options: ComboboxOption[];
  /** The part of the value the suggestions complete, and the value with a
   * suggestion in its place. By default, the whole value. */
  complete?: (value: string) => { query: string; replace: (option: string) => string };
  "aria-label": string;
  placeholder?: string;
  disabled?: boolean;
}) {
  const id = createUniqueId();
  const [open, setOpen] = createSignal(false);
  const [cursor, setCursor] = createSignal(-1);
  const [position, setPosition] = createSignal({ left: 0, top: 0, width: 280 });
  let field!: HTMLDivElement;
  let input!: HTMLInputElement;
  let popup: HTMLDivElement | undefined;
  const completion = () =>
    props.complete?.(props.value) ?? {
      query: props.value.trim(),
      replace: (option: string) => option,
    };
  const matches = createMemo(() => {
    const query = completion().query.toLocaleLowerCase();
    if (!query) return props.options;
    return props.options.filter((option) =>
      `${option.value} ${option.label ?? ""}`.toLocaleLowerCase().includes(query),
    );
  });
  const visible = () => open() && !props.disabled && matches().length > 0;
  // A highlight that no longer names a suggestion is dropped, so Enter goes
  // back to the form instead of putting in something unseen.
  createEffect(on(matches, (list) => cursor() >= list.length && setCursor(-1)));
  const close = () => {
    setOpen(false);
    setCursor(-1);
  };
  const pick = (option: ComboboxOption) => {
    props.onInput(completion().replace(option.value));
    close();
    input.focus({ preventScroll: true });
  };
  const reveal = () =>
    queueMicrotask(() =>
      popup?.querySelector(`#${CSS.escape(`${id}-${cursor()}`)}`)?.scrollIntoView({ block: "nearest" }),
    );
  const move = (step: number) => {
    const count = matches().length;
    if (!count) return;
    if (!open()) {
      setOpen(true);
      setCursor(step > 0 ? 0 : count - 1);
    } else setCursor((index) => (index < 0 ? (step > 0 ? 0 : count - 1) : (index + step + count) % count));
    reveal();
  };
  createEffect(
    on(visible, (shown) => {
      if (!shown) return;
      const place = () => {
        const anchor = field.getBoundingClientRect();
        const { left, width } = floatingBox(anchor, Math.max(anchor.width, 260), 220);
        const height = popup?.getBoundingClientRect().height ?? 0;
        setPosition({
          left,
          width,
          top:
            anchor.bottom + height + 6 <= window.innerHeight - 8
              ? anchor.bottom + 4
              : Math.max(8, anchor.top - height - 4),
        });
      };
      place();
      queueMicrotask(place);
      const outside = (event: Event) => {
        const path = event.composedPath();
        if (!path.includes(field) && (!popup || !path.includes(popup))) close();
      };
      document.addEventListener("pointerdown", outside, true);
      document.addEventListener("scroll", place, true);
      window.addEventListener("resize", place);
      onCleanup(() => {
        document.removeEventListener("pointerdown", outside, true);
        document.removeEventListener("scroll", place, true);
        window.removeEventListener("resize", place);
      });
    }),
  );
  return (
    <>
      <div ref={field} class="combobox">
        <input
          ref={input}
          role="combobox"
          aria-label={props["aria-label"]}
          aria-autocomplete="list"
          aria-expanded={visible()}
          aria-controls={visible() ? id : undefined}
          aria-activedescendant={visible() && cursor() >= 0 ? `${id}-${cursor()}` : undefined}
          autocomplete="off"
          spellcheck={false}
          placeholder={props.placeholder}
          disabled={props.disabled}
          value={props.value}
          onInput={(event) => {
            props.onInput(event.currentTarget.value);
            setCursor(-1);
            setOpen(true);
          }}
          onBlur={close}
          onKeyDown={(event) => {
            if (event.isComposing) return;
            if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              event.preventDefault();
              move(event.key === "ArrowDown" ? 1 : -1);
            } else if (event.key === "Escape" && visible()) {
              event.preventDefault();
              event.stopPropagation();
              close();
            } else if (event.key === "Enter" && visible() && cursor() >= 0) {
              event.preventDefault();
              pick(matches()[cursor()]);
            } else if (event.key === "Enter" || event.key === "Tab") close();
          }}
        />
        <button
          type="button"
          class="combobox-toggle"
          tabIndex={-1}
          aria-label={t("Show suggestions")}
          disabled={props.disabled || !props.options.length}
          onPointerDown={(event) => event.preventDefault()}
          onClick={() => {
            input.focus({ preventScroll: true });
            if (visible()) close();
            else {
              setOpen(true);
              setCursor(-1);
            }
          }}
        >
          <Icon name="down" size="sm" />
        </button>
      </div>
      <Show when={visible()}>
        <Portal mount={field.closest('dialog, [role="dialog"]') ?? document.body}>
          <div
            ref={popup}
            class="select-popover combobox-popover"
            style={{
              left: position().left + "px",
              top: position().top + "px",
              width: position().width + "px",
            }}
          >
            <div id={id} class="select-options" role="listbox" aria-label={props["aria-label"]}>
              <For each={matches()}>
                {(option, index) => (
                  <div
                    id={`${id}-${index()}`}
                    role="option"
                    aria-selected={cursor() === index()}
                    class="select-option"
                    classList={{ highlighted: cursor() === index() }}
                    onPointerDown={(event) => event.preventDefault()}
                    onPointerMove={() => setCursor(index())}
                    onClick={() => pick(option)}
                  >
                    <span class="combobox-value">{option.value}</span>
                    <Show when={option.label}>
                      <span class="combobox-detail">{option.label}</span>
                    </Show>
                  </div>
                )}
              </For>
            </div>
          </div>
        </Portal>
      </Show>
    </>
  );
}
