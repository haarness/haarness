import { t } from "./i18n";
import {
  For,
  Show,
  createEffect,
  createSignal,
  onCleanup,
  untrack,
} from "solid-js";
import type {
  ManagementEntry,
  ManagementField,
  ManagementView,
} from "@haarness/sdk";
import { useApp } from "./state";
import { Select } from "./Select";
import { ProviderLabel } from "./ProviderIcon";
import { ErrorNotice } from "./ui";
import { cancelled } from "./error-text";
import { iconFailed, iconSource, rowsHaveIcons } from "./entry-icon";
import "./management.css";

/**
 * The square image a view names beside its title or a row. It fades in once
 * loaded; missing, refused by the page's CSP or failing to load, it is gone,
 * never a broken-image box.
 */
function EntryIcon(props: { icon?: string | null; class: string; onFail?: () => void }) {
  const [loaded, setLoaded] = createSignal(false);
  const [broken, setBroken] = createSignal(false);
  return (
    <Show when={!broken() && iconSource(props.icon)} keyed>
      {(src) => (
        <img
          class={props.class}
          classList={{ loaded: loaded() }}
          src={src}
          alt=""
          loading="lazy"
          decoding="async"
          referrerpolicy="no-referrer"
          draggable={false}
          ref={(image) => {
            if (image.complete && image.naturalWidth > 0) setLoaded(true);
          }}
          onLoad={() => setLoaded(true)}
          onError={() => {
            iconFailed(src);
            setBroken(true);
            props.onFail?.();
          }}
        />
      )}
    </Show>
  );
}

/** A sentence reads as text; JSON, commands and paths keep the code face. */
const prose = (line: string) =>
  !line.includes("\n") && /\s/.test(line) && !/^[[{$>]|[{}]|^\S+=/.test(line.trim());
/** Long or structured values get a text area; names, URLs and searches a field. */
const multiline = (field: ManagementField) =>
  field.value.includes("\n") ||
  /json|content|hostnames|arguments|instructions|prompt|environment|headers|body|description|values/i.test(
    field.label,
  );
/** A form's button says what it does. */
function submitLabel(submit: unknown) {
  const action = (submit as Record<string, unknown> | undefined)?.action;
  if (action === "router_connect") return t("Connect");
  if (action === "search") return t("Search");
  if (action === "install") return t("Install");
  return t("Save");
}
/** Server-owned forms keep remote and local mutations on the same domain API. */
export function ManagementPanel(props: {
  resource: string;
  threadId?: string;
  pending?: (value: boolean) => void;
  initialRequest?: Record<string, unknown>;
  /** The settings page already names this resource; repeat it only in sub-views. */
  rootHeading?: boolean;
}) {
  const app = useApp();
  const [view, setView] = createSignal<ManagementView>();
  const [values, setValues] = createSignal<Record<string, string>>({});
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [confirmation, setConfirmation] = createSignal<ManagementEntry>();
  // The list a "Back to list" returns to; on that list the button goes nowhere.
  const [rootTitle, setRootTitle] = createSignal<string>();
  const [atRoot, setAtRoot] = createSignal(false);
  // A row's icon that fails may leave the list with none to line up.
  const [iconFailures, setIconFailures] = createSignal(0);
  const withIcons = (view: ManagementView) => {
    void iconFailures();
    return rowsHaveIcons(view.entries?.filter((entry) => entry.description) ?? []);
  };
  let generation = 0;
  onCleanup(() => {
    generation++;
    props.pending?.(false);
  });
  async function request(input: unknown, reset = false) {
    const id = app.state.connection?.id,
      resource = props.resource,
      threadId = props.threadId;
    if (!id || (busy() && !reset)) return;
    const current = ++generation;
    setBusy(true);
    props.pending?.(true);
    setError("");
    setConfirmation(undefined);
    if (reset) {
      setView(undefined);
      setValues({});
    }
    try {
      const result = await window.haarness.api<ManagementView>(
        id,
        "manageResource",
        [resource, { ...(input as object), ...(threadId ? { threadId } : {}) }],
      );
      if (current !== generation || id !== app.state.connection?.id) return;
      const action = (input as Record<string, unknown>)?.action;
      if (action === "view") setRootTitle(result.title);
      // Searching or toggling a row re-renders the same list in place. Any
      // other action (a save, an inspect) shows a different page, even when
      // the server gives it the list's title.
      setAtRoot(
        action === "view" ||
          (["search", "enable", "disable"].includes(String(action)) &&
            result.title === rootTitle()),
      );
      setView(result);
      setValues(
        Object.fromEntries(
          (result.fields ?? []).map((field) => [field.key, field.value]),
        ),
      );
    } catch (cause) {
      if (current === generation)
        if (!cancelled(cause)) setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (current === generation) {
        setBusy(false);
        props.pending?.(false);
      }
    }
  }
  createEffect(() => {
    void app.state.connection?.id;
    void props.resource;
    void props.threadId;
    untrack(() => void request(props.initialRequest ?? { action: "view" }, true));
  });
  const provider = (entry: ManagementEntry) => {
    const request = entry.request as Record<string, unknown> | null;
    return props.resource === "providers" &&
      request?.action === "inspect" &&
      typeof request.name === "string"
      ? request.name
      : undefined;
  };
  const entrySetting = (entry: ManagementEntry) => {
    const request = entry.request as Record<string, unknown> | null;
    return request?.action === "inspect"
      ? (props.resource === "mcp"
          ? "mcp"
          : props.resource === "providers"
            ? "provider"
            : props.resource) +
          ":" +
          (request.name ?? request.id)
      : entry.label;
  };
  const activate = (entry: ManagementEntry) => {
    const input = entry.request as Record<string, unknown>;
    const draft = input.action === "router_form"
      ? Object.fromEntries(Object.entries(values()).filter(([key]) => key !== "key"))
      : {};
    return entry.confirm ? setConfirmation(entry) : void request({ ...input, ...draft });
  };
  return (
    <section class="management-panel" classList={{ "router-connection-panel": view()?.title === "Connect Haarness Router" || view()?.title === "Router connected" }} aria-busy={busy()}>
      <Show when={error()}>
        <ErrorNotice error={error()} />
      </Show>
      <Show
        when={view()}
        fallback={
          // A failed first load already said why above; offer the retry
          // rather than a second, vaguer line.
          <Show
            when={!busy() && error()}
            fallback={
              <p class="muted">
                {busy() ? t("Loading…") : t("Configuration is unavailable.")}
              </p>
            }
          >
            <div class="management-actions">
              <button
                type="button"
                onClick={() => void request(props.initialRequest ?? { action: "view" }, true)}
              >
                {t("Try again")}
              </button>
            </div>
          </Show>
        }
        keyed
      >
        {(current) => (
          <>
            <Show
              when={!atRoot() || props.rootHeading !== false}
            >
              <header class="management-card-heading">
                <EntryIcon icon={current.icon} class="management-title-icon" />
                <h3>{current.title}</h3>
                <Show when={!atRoot()}>
                  <button
                    type="button"
                    disabled={busy()}
                    onClick={() => void request({ action: "view" })}
                  >
                    {t("Back to list")}
                  </button>
                </Show>
              </header>
            </Show>
            <For each={current.lines}>
              {(line) =>
                /^https?:\/\/\S+$/.test(line) ? (
                  <button
                    type="button"
                    class="management-source"
                    onClick={() => void window.haarness.openExternal(line)}
                  >
                    {line}
                  </button>
                ) : prose(line) ? (
                  <p class="management-text">{line}</p>
                ) : (
                  <pre class="management-source">{line}</pre>
                )
              }
            </For>
            <fieldset disabled={busy()}>
              <For each={current.fields}>
                {(field) => (
                  <label class="field">
                    <span>{field.label}</span>
                    <Show
                      when={field.choices?.length}
                      fallback={
                        <Show
                          when={field.secret || !multiline(field)}
                          fallback={
                            <textarea
                              rows={
                                field.value.includes("\n") ||
                                /content|hostnames|arguments/i.test(field.label)
                                  ? 5
                                  : 2
                              }
                              aria-label={field.label}
                              value={values()[field.key] ?? ""}
                              required={field.required}
                              onInput={(event) =>
                                setValues({
                                  ...values(),
                                  [field.key]: event.currentTarget.value,
                                })
                              }
                            />
                          }
                        >
                          <input
                            type={field.secret ? "password" : "text"}
                            autocomplete={field.secret ? "off" : undefined}
                            aria-label={field.label}
                            value={values()[field.key] ?? ""}
                            required={field.required}
                            onInput={(event) =>
                              setValues({
                                ...values(),
                                [field.key]: event.currentTarget.value,
                              })
                            }
                            onKeyDown={(event) => {
                              if (
                                event.key !== "Enter" ||
                                event.isComposing ||
                                !current.submit
                              )
                                return;
                              event.preventDefault();
                              void request({
                                ...(current.submit as object),
                                ...values(),
                              });
                            }}
                          />
                        </Show>
                      }
                    >
                      <Select
                        aria-label={field.label}
                        value={values()[field.key]}
                        options={(field.choices ?? []).map((value) => ({
                          value,
                          label: value,
                        }))}
                        onChange={(value) =>
                          setValues({ ...values(), [field.key]: value })
                        }
                      />
                    </Show>
                  </label>
                )}
              </For>
              <Show when={current.submit}>
                <button
                  type="button"
                  class="primary"
                  onClick={() =>
                    void request({ ...(current.submit as object), ...values() })
                  }
                >
                  {submitLabel(current.submit)}
                </button>
              </Show>
              <Show when={current.entries?.some((entry) => entry.description)}>
                <div
                  class="management-entries"
                  classList={{ "with-icons": withIcons(current) }}
                >
                  <For each={current.entries?.filter((entry) => entry.description)}>
                    {(entry) => (
                      <button
                        type="button"
                        class="management-entry"
                        aria-label={entry.label}
                        aria-description={entry.description}
                        data-setting={entrySetting(entry)}
                        onClick={() => activate(entry)}
                      >
                        <Show when={withIcons(current)}>
                          <span class="management-entry-icon" aria-hidden="true">
                            <EntryIcon
                              icon={entry.icon}
                              class="management-icon"
                              onFail={() => setIconFailures(iconFailures() + 1)}
                            />
                          </span>
                        </Show>
                        <span class="management-entry-label">
                          <Show when={provider(entry)} fallback={entry.label}>
                            <ProviderLabel provider={provider(entry)}>
                              {entry.label}
                            </ProviderLabel>
                          </Show>
                        </span>
                        <span class="management-entry-description">
                          {entry.description}
                        </span>
                      </button>
                    )}
                  </For>
                </div>
              </Show>
              <div class="management-actions">
                <For each={current.entries?.filter((entry) => !entry.description)}>
                  {(entry) => (
                    <button
                      type="button"
                      data-setting={entrySetting(entry)}
                      onClick={() => activate(entry)}
                    >
                      <Show when={provider(entry)} fallback={entry.label}>
                        <ProviderLabel provider={provider(entry)}>
                          {entry.label}
                        </ProviderLabel>
                      </Show>
                    </button>
                  )}
                </For>
              </div>
            </fieldset>
            <Show when={confirmation()} keyed>
              {(entry) => (
                <div
                  role="alertdialog"
                  aria-label={entry.confirm ?? entry.label}
                  class="management-card"
                >
                  <p>{entry.confirm}</p>
                  <p class="muted">{entry.description}</p>
                  <div class="management-actions">
                    <button
                      type="button"
                      onClick={() => setConfirmation(undefined)}
                    >
                      {t("Cancel")}
                    </button>
                    {/* The server asks first only before a destructive
                        entry, so the button repeats that entry's action. */}
                    <button
                      type="button"
                      class="danger-action"
                      onClick={() => void request(entry.request)}
                    >
                      {entry.label.charAt(0).toUpperCase() + entry.label.slice(1)}
                    </button>
                  </div>
                </div>
              )}
            </Show>
          </>
        )}
      </Show>
    </section>
  );
}
