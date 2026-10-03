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
import "./management.css";

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
      if ((input as Record<string, unknown>)?.action === "view")
        setRootTitle(result.title);
      setView(result);
      setValues(
        Object.fromEntries(
          (result.fields ?? []).map((field) => [field.key, field.value]),
        ),
      );
    } catch (cause) {
      if (current === generation)
        setError(cause instanceof Error ? cause.message : String(cause));
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
        <p role="alert" class="form-error">
          {error()}
        </p>
      </Show>
      <Show
        when={view()}
        fallback={
          <p class="muted">
            {busy() ? t("Loading…") : t("Configuration is unavailable.")}
          </p>
        }
        keyed
      >
        {(current) => (
          <>
            <Show
              when={current.title !== rootTitle() || props.rootHeading !== false}
            >
              <header class="management-card-heading">
                <h3>{current.title}</h3>
                <Show when={current.title !== rootTitle()}>
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
                <div class="management-entries">
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
                    <button
                      type="button"
                      class="primary"
                      onClick={() => void request(entry.request)}
                    >
                      {t("Confirm")}
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
