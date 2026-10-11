import { language, t } from "./i18n";
import { Select } from "./Select";
import { For, Show, createSignal, onMount, type JSX } from "solid-js";
import { Icon, type IconName, type IconSize } from "./icons";
import { errorText } from "./error-text";
export { Icon, type IconName } from "./icons";
/**
 * The one icon-only control. It owns the hit area (`--icon-button`), the
 * accessible name and the tooltip, so no screen ever ships a bare `<svg>` in a
 * button. `size` picks a smaller *glyph*, never a smaller target.
 */
export const Button = (props: {
  label: string;
  icon: IconName;
  onClick: () => void;
  active?: boolean;
  disabled?: boolean;
  class?: string;
  size?: IconSize;
  // Extra explanation for the tooltip; the accessible name stays terse.
  hint?: string;
}) => (
  <button
    type="button"
    class={"icon-button " + (props.class ?? "")}
    classList={{ active: props.active }}
    title={props.hint ? props.label + " — " + props.hint : props.label}
    aria-label={props.label}
    aria-pressed={props.active === undefined ? undefined : props.active}
    onClick={props.onClick}
    disabled={props.disabled}
  >
    <Icon name={props.icon} size={props.size} />
  </button>
);
export interface Field {
  name: string;
  label: string;
  value?: string;
  placeholder?: string;
  type?: "text" | "textarea" | "password" | "number" | "select" | "checkbox";
  /** Fixed choices, or an accessor for choices that arrive after the dialog
   * opens; the control then settles on the first one. */
  options?: string[] | (() => string[]);
  required?: boolean;
  readonly?: boolean;
}
/** One line of a dialog's list: shown as written (a path, an id), with an
 * optional short tag that is already translated. */
export interface ListItem {
  text: string;
  tag?: string;
}
interface Form {
  title: string;
  description?: string;
  /** What the action touches, under the description. */
  list?: ListItem[];
  fields: Field[];
  submit: string;
  /** The submit removes, deletes or discards something: style it as such. */
  danger?: boolean;
  onSubmit?: (value: Record<string, string>) => Promise<unknown>;
  resolve: (value: Record<string, string> | null) => void;
}
const [form, setForm] = createSignal<Form | null>(null);
/**
 * Open the shared form dialog. Pass English catalog keys (or text already
 * translated with interpolated values): the dialog translates its title,
 * description, field labels, placeholders and submit label when it renders.
 */
export function ask(
  title: string,
  fields: Field[],
  options: { description?: string; list?: ListItem[]; submit?: string; danger?: boolean; onSubmit?: Form["onSubmit"] } = {},
) {
  return new Promise<Record<string, string> | null>((resolve) =>
    setForm({
      title,
      fields,
      description: options.description,
      list: options.list,
      onSubmit: options.onSubmit,
      submit: options.submit ?? "Save",
      danger: options.danger,
      resolve,
    }),
  );
}
/**
 * Ask before an action. The submit names the action ("Remove project",
 * "Disconnect"), never a bare "Confirm", and `danger` marks one that removes,
 * deletes or discards something.
 */
export async function confirm(
  title: string,
  description: string,
  options: { submit: string; danger?: boolean; list?: ListItem[] },
) {
  return !!(await ask(title, [], { description, ...options }));
}

/** Dismiss only a complete backdrop click, never a drag out of the panel. */
export function dismissOnBackdrop(close: () => void) {
  let pressedBackdrop = false;
  const outside = (event: MouseEvent & { currentTarget: HTMLElement }) => {
    if (event.target !== event.currentTarget) return false;
    // Native dialogs receive backdrop events on themselves. Custom backdrops
    // can also receive a click when a drag ends inside their panel. Check the
    // panel box in both cases; padding and drag releases inside stay open.
    const panel =
      event.currentTarget instanceof HTMLDialogElement
        ? event.currentTarget
        : event.currentTarget.querySelector<HTMLElement>('[role="dialog"]');
    if (!panel) return false;
    const box = panel.getBoundingClientRect();
    return (
      event.clientX < box.left ||
      event.clientX > box.right ||
      event.clientY < box.top ||
      event.clientY > box.bottom
    );
  };
  return {
    onPointerDown(event: PointerEvent & { currentTarget: HTMLElement }) {
      pressedBackdrop = event.button === 0 && event.isPrimary && outside(event);
    },
    onPointerCancel() {
      pressedBackdrop = false;
    },
    onClick(event: MouseEvent & { currentTarget: HTMLElement }) {
      const dismiss = pressedBackdrop && event.button === 0 && outside(event);
      pressedBackdrop = false;
      if (dismiss) {
        event.preventDefault();
        event.stopPropagation();
        close();
      }
    },
  };
}

/**
 * Start a form on its first field, not on the header's Close button that the
 * browser would pick, with the text selected so typing replaces it. A field
 * marked `autofocus` keeps its focus; a form without fields starts on
 * `fallback`.
 */
export function focusFirstField(dialog: HTMLElement, fallback?: HTMLElement | null) {
  if (dialog.querySelector("[autofocus]")) return;
  const field =
    dialog.querySelector<HTMLElement>(
      "fieldset :is(input:not([type=hidden]), textarea, button):not(:disabled, [readonly])",
    ) ?? fallback;
  field?.focus();
  if (field instanceof HTMLInputElement && field.type !== "checkbox") field.select();
}
export function Forms() {
  let dialog!: HTMLDialogElement;
  return (
    <Show when={form()} keyed>
      {(current) => {
        const [busy, setBusy] = createSignal(false);
        const [error, setError] = createSignal("");
        const finish = (value: Record<string, string> | null) => {
          // A dismissed submission may finish after another form opens.
          if (form() !== current) return;
          dialog.close();
          setForm(null);
          current.resolve(value);
        };
        onMount(() => {
          dialog.showModal();
          // A confirmation has no field: start on Cancel, so Enter never
          // removes or deletes anything by accident.
          focusFirstField(dialog, dialog.querySelector<HTMLElement>("footer button"));
        });
        return (
          <dialog
            ref={dialog}
            class="dialog"
            aria-label={t(current.title)}
            onCancel={(event) => { event.preventDefault(); finish(null); }}
            {...dismissOnBackdrop(() => finish(null))}
          >
            <form
              onSubmit={async (event) => {
                event.preventDefault();
                if (busy()) return;
                const input = Object.fromEntries(new FormData(event.currentTarget).entries()) as Record<string, string>;
                const focused = document.activeElement;
                setError("");
                setBusy(true);
                try {
                  await current.onSubmit?.(input);
                  setBusy(false);
                  finish(input);
                } catch (error) {
                  setError(error instanceof Error ? error.message : String(error));
                  setBusy(false);
                  // Disabling the form while it saved dropped focus; give it
                  // back so the field can be corrected straight away.
                  if (focused instanceof HTMLElement && dialog.contains(focused)) focused.focus();
                }
              }}
            >
              <header>
                <h2>{t(current.title)}</h2>
                <Button
                  icon="close"
                  label={t("Close dialog")}
                  onClick={() => finish(null)}
                />
              </header>
              <Show when={current.description}>
                {(description) => <p class="muted">{t(description())}</p>}
              </Show>
              <Show when={current.list?.length}>
                <ul class="dialog-list">
                  <For each={current.list}>
                    {(item) => (
                      <li>
                        <span dir="auto">{item.text}</span>
                        <Show when={item.tag}>{(tag) => <span class="badge dialog-list-tag">{tag()}</span>}</Show>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
              <fieldset disabled={busy()}>
              <For each={current.fields}>
                {(field) => {
                  const choices = () =>
                    (typeof field.options === "function" ? field.options() : field.options) ?? [];
                  return (
                  <label class="field">
                    <span>{t(field.label)}</span>
                    <Show
                      when={field.type === "textarea"}
                      fallback={
                        <Show
                          when={field.type === "select"}
                          fallback={
                            <input
                              name={field.name}
                              type={field.type ?? "text"}
                              value={field.value ?? ""}
                              placeholder={field.placeholder && t(field.placeholder)}
                              required={field.required}
                              readOnly={field.readonly}
                            />
                          }
                        >
                          <Select
                            name={field.name}
                            aria-label={t(field.label)}
                            defaultValue={field.value ?? choices()[0]}
                            options={choices().map((value) => ({
                              // Fixed choices are English keys the caller
                              // compares against; only their label translates.
                              value,
                              label: t(value),
                            }))}
                          />
                        </Show>
                      }
                    >
                      <textarea
                        name={field.name}
                        rows={5}
                        value={field.value ?? ""}
                        placeholder={field.placeholder && t(field.placeholder)}
                        required={field.required}
                        readOnly={field.readonly}
                      />
                    </Show>
                  </label>
                  );
                }}
              </For>
              </fieldset>
              <Show when={error()}><ErrorNotice class="notice-inline" error={error()} /></Show>
              <footer>
                <button type="button" onClick={() => finish(null)}>
                  {t("Cancel")}
                </button>
                <button
                  type="submit"
                  class={current.danger ? "danger-action" : "primary"}
                  disabled={busy()}
                >
                  {t(current.submit)}
                </button>
              </footer>
            </form>
          </dialog>
        );
      }}
    </Show>
  );
}
/**
 * A failure in words: a title when one helps, then what happened. Copy that
 * stands in for the server's own text keeps that text as the tooltip.
 */
export function ErrorNotice(props: {
  error: unknown;
  class?: string;
  ref?: (element: HTMLParagraphElement) => void;
  focusable?: boolean;
}) {
  const text = () => errorText(props.error);
  const say = (value: string) => (text().known ? t(value) : value);
  return (
    <p
      ref={props.ref}
      class={props.class ?? "form-error"}
      role="alert"
      tabindex={props.focusable ? -1 : undefined}
      title={text().detail}
    >
      <Show when={text().title}>{(title) => <strong>{say(title())}</strong>}</Show>
      {say(text().message)}
    </p>
  );
}
export const Empty = (props: {
  title: string;
  detail?: string;
  children?: JSX.Element;
}) => (
  <div class="empty">
    {/* Like the form dialog, an empty state takes English keys and translates them. */}
    <h2>{t(props.title)}</h2>
    <p>{props.detail && t(props.detail)}</p>
    {props.children}
  </div>
);
export function stringify(value: unknown) {
  return typeof value === "string" ? value : JSON.stringify(value, null, 2);
}
/** "5:51 PM" today; "Oct 2, 5:51 PM" for anything older, so a long
 * conversation never shows two identical-looking times from different days. */
export function time(value: string) {
  const date = new Date(value);
  const today = date.toDateString() === new Date().toDateString();
  return date.toLocaleString(language(), {
    ...(today ? {} : { month: "short", day: "numeric" }),
    hour: "numeric",
    minute: "2-digit",
  });
}
