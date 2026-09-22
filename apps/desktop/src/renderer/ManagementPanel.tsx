import { t } from "./i18n";
import {
  For,
  Show,
  createEffect,
  createSignal,
  onCleanup,
  untrack,
} from "solid-js";
import type { ManagementEntry, ManagementView } from "@haarness/sdk";
import { useApp } from "./state";
import { Select } from "./Select";
import { ProviderLabel } from "./ProviderIcon";
import "./management.css";

/** Server-owned forms keep remote and local mutations on the same domain API. */
export function ManagementPanel(props: {
  resource: string;
  threadId?: string;
  pending?: (value: boolean) => void;
  initialRequest?: Record<string, unknown>;
}) {
  const app = useApp();
  const [view, setView] = createSignal<ManagementView>();
  const [values, setValues] = createSignal<Record<string, string>>({});
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [confirmation, setConfirmation] = createSignal<ManagementEntry>();
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
            <header class="management-card-heading">
              <h3>{current.title}</h3>
              <button
                type="button"
                disabled={busy()}
                onClick={() => void request({ action: "view" })}
              >
                {t("Back to list")}
              </button>
            </header>
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
                          when={field.secret || field.key === "name" || ((current.submit as Record<string, unknown> | undefined)?.action === "router_connect" && ["url", "label"].includes(field.key))}
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
                  {(current.submit as Record<string, unknown>).action ===
                  "router_connect"
                    ? t("Connect")
                    : t("Save")}
                </button>
              </Show>
              <div class="management-actions">
                <For each={current.entries}>
                  {(entry) => (
                    <button
                      type="button"
                      data-setting={(entry.request as Record<string, unknown>)?.action === "inspect" ? (props.resource === "mcp" ? "mcp" : props.resource === "providers" ? "provider" : props.resource) + ":" + ((entry.request as Record<string, unknown>).name ?? (entry.request as Record<string, unknown>).id) : entry.label}
                      title={entry.description}
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
