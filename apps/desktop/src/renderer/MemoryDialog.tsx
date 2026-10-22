import { t } from "./i18n";
import { ManagementPanel } from "./ManagementPanel";
import { For, Show, createSignal, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import { createStore } from "solid-js/store";
import { FormDialog } from "./FormDialog";
import { Select } from "./Select";
import { useApp } from "./state";
import { confirm } from "./ui";
import "./management.css";
interface Memory {
  name: string;
  description: string;
  type: string;
  content?: string;
  revision?: string;
}
interface Status {
  config: { enabled: boolean; indexLimit: number };
  /** The settings file is there but could not be read: memory stays off. */
  settingsUnreadable?: boolean;
  directory: string;
  revision: string;
  memories: Memory[];
}
export function MemoryDialog(props: { close: () => void }) {
  const app = useApp();
  const connection = app.state.connection!;
  const request = <T,>(input: Record<string, unknown>) =>
    window.haarness.native<T>(connection.id, {
      action: "asset",
      kind: "memory",
      input,
    });
  const [state, setState] = createSignal<Status>();
  const [enabled, setEnabled] = createSignal(true),
    [limit, setLimit] = createSignal("200");
  const [error, setError] = createSignal(""),
    [busy, setBusy] = createSignal(false),
    [notice, setNotice] = createSignal("");
  const [editor, setEditor] = createSignal<{ item?: Memory }>();
  function accept(value: Status) {
    setState(value);
    setEnabled(value.config.enabled);
    setLimit(String(value.config.indexLimit));
  }
  async function run(operation: () => Promise<void>) {
    if (busy()) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await operation();
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }
  const load = async () =>
    accept(await request<Status>({ action: "status" }));
  onMount(() => {
    if (!connection.remote) void run(load);
  });
  return (
    <FormDialog
      title={t("Memory")}
      pending={busy()}
      description={t("Memory for {project}. Stored outside the repository.", {
        project: connection.label,
      })}
      close={props.close}
      save={async () => {}}
    >
      {() => (
        <div class="management-panel">
          <Show
            when={!connection.remote}
            fallback={
              <ManagementPanel resource="memory" pending={setBusy} />
            }
          >
            <Show when={error()}>
              <p class="form-error" role="alert">
                {error()}
              </p>
            </Show>
            <Show when={notice()}>
              <p class="muted" role="status">
                {notice()}
              </p>
            </Show>
            <Show
              when={state()}
              fallback={
                <p class="muted">
                  {busy()
                    ? t("Loading memory…")
                    : t("Memory could not be loaded.")}
                </p>
              }
            >
              <Show when={state()?.settingsUnreadable}>
                <p class="muted" role="status">
                  {t(
                    "Memory is off because its settings file could not be read. Saving replaces it.",
                  )}
                </p>
              </Show>
              <label class="memory-setting">
                <span>
                  <strong>{t("Enable project memory")}</strong>
                  <small class="muted management-source">
                    {t(
                      "Include the memory index in prompts and make memory tools available.",
                    )}
                  </small>
                </span>
                <input
                  type="checkbox"
                  role="switch"
                  aria-label={t("Enable project memory")}
                  checked={enabled()}
                  disabled={busy()}
                  onChange={(e) => setEnabled(e.currentTarget.checked)}
                />
              </label>
              <label class="memory-setting">
                <span>
                  <strong>{t("Index limit")}</strong>
                  <small class="muted management-source">
                    {t("Maximum memory summaries included in the prompt.")}
                  </small>
                </span>
                <input
                  aria-label={t("Memory index limit")}
                  type="number"
                  min="1"
                  max="1000"
                  step="1"
                  value={limit()}
                  disabled={busy()}
                  onInput={(e) => setLimit(e.currentTarget.value)}
                />
              </label>
              <div class="management-actions">
                <button
                  type="button"
                  class="primary"
                  disabled={busy()}
                  onClick={() =>
                    void run(async () => {
                      const indexLimit = Number(limit());
                      if (
                        !Number.isInteger(indexLimit) ||
                        indexLimit < 1 ||
                        indexLimit > 1000
                      )
                        throw Error(
                          t("Index limit must be between 1 and 1000."),
                        );
                      accept(
                        await request<Status>({
                          action: "configure",
                          revision: state()!.revision,
                          config: { enabled: enabled(), indexLimit },
                        }),
                      );
                      setNotice(
                        t(
                          "Memory settings saved. Runtime changes apply when idle.",
                        ),
                      );
                    })
                  }
                >
                  {t("Save memory settings")}
                </button>
              </div>
              <small class="muted management-source">
                {state()!.directory}
              </small>
              <div class="management-card-heading">
                <strong>
                  {state()!.memories.length === 1
                    ? t("{count} memory", { count: state()!.memories.length })
                    : t("{count} memories", {
                        count: state()!.memories.length,
                      })}
                </strong>
                <button
                  type="button"
                  disabled={busy()}
                  onClick={() => setEditor({})}
                >
                  {t("Add memory")}
                </button>
              </div>
              <div class="memory-list">
                <For each={state()!.memories}>
                  {(item) => (
                    <article class="management-card">
                      <strong>{item.name}</strong>
                      <span class="muted">
                        {item.type} · {item.description}
                      </span>
                      <div class="management-actions">
                        <button
                          type="button"
                          disabled={busy()}
                          aria-label={t("Edit memory {name}", {
                            name: item.name,
                          })}
                          onClick={() =>
                            void run(async () => {
                              setEditor({
                                item: await request<Memory>({
                                  action: "read",
                                  name: item.name,
                                }),
                              });
                            })
                          }
                        >
                          {t("Edit memory")}
                        </button>
                      </div>
                    </article>
                  )}
                </For>
              </div>
            </Show>
            <button
              type="button"
              disabled={busy()}
              onClick={() => void run(load)}
            >
              {t("Refresh memory")}
            </button>
            <Show when={editor()} keyed>
              {(value) => (
                <Portal>
                  <MemoryEditor
                    item={value.item}
                    close={() => setEditor(undefined)}
                    save={async (input) => {
                      await request(input);
                      await load();
                    }}
                    remove={async () => {
                      if (value.item)
                        await request({
                          action: "delete",
                          name: value.item.name,
                          revision: value.item.revision,
                        });
                      await load();
                    }}
                  />
                </Portal>
              )}
            </Show>
          </Show>
        </div>
      )}
    </FormDialog>
  );
}
function MemoryEditor(props: {
  item?: Memory;
  close: () => void;
  save: (input: Record<string, unknown>) => Promise<void>;
  remove: () => Promise<void>;
}) {
  const [form, setForm] = createStore({
    name: props.item?.name ?? "",
    description: props.item?.description ?? "",
    type: props.item?.type ?? "project",
    content: props.item?.content ?? "",
  });
  let dismissed = false;
  const close = () => {
    if (dismissed) return;
    dismissed = true;
    props.close();
  };
  const [deleting, setDeleting] = createSignal(false),
    [error, setError] = createSignal("");
  return (
    <FormDialog
      title={props.item ? t("Edit memory") : t("Add memory")}
      submit={t("Save memory")}
      pending={deleting()}
      close={close}
      save={async () => {
        if (deleting()) throw Error(t("Wait for deletion to finish."));
        if (
          !form.name.trim() ||
          !form.description.trim() ||
          !form.content.trim()
        )
          throw Error(t("Add a name, description, and memory content."));
        await props.save({
          action: props.item ? "save" : "create",
          ...form,
          revision: props.item?.revision,
        });
      }}
    >
      {(busy) => (
        <>
          <label class="field">
            <span>{t("Name")}</span>
            <input
              aria-label={t("Memory name")}
              value={form.name}
              readOnly={!!props.item}
              disabled={deleting()}
              onInput={(e) => setForm("name", e.currentTarget.value)}
            />
          </label>
          <label class="field">
            <span>{t("Description")}</span>
            <input
              aria-label={t("Memory description")}
              value={form.description}
              disabled={deleting()}
              onInput={(e) => setForm("description", e.currentTarget.value)}
            />
          </label>
          <label class="field">
            <span>{t("Type")}</span>
            <Select
              aria-label={t("Memory type")}
              value={form.type}
              disabled={busy() || deleting()}
              options={["user", "feedback", "project", "reference"].map(
                (value) => ({ value, label: value }),
              )}
              onChange={(value) => setForm("type", value)}
            />
          </label>
          <label class="field">
            <span>{t("Content")}</span>
            <textarea
              aria-label={t("Memory content")}
              rows={10}
              value={form.content}
              disabled={deleting()}
              onInput={(e) => setForm("content", e.currentTarget.value)}
            />
          </label>
          <Show when={props.item}>
            <button
              type="button"
              disabled={busy() || deleting()}
              onClick={async () => {
                if (!(await confirm(t("Delete memory?"), form.name, { submit: t("Delete memory"), danger: true }))) return;
                setDeleting(true);
                try {
                  await props.remove();
                  setDeleting(false);
                  close();
                } catch (error) {
                  setDeleting(false);
                  setError(
                    error instanceof Error ? error.message : String(error),
                  );
                }
              }}
            >
              {t("Delete memory")}
            </button>
          </Show>
          <Show when={error()}>
            <p role="alert" class="form-error">
              {error()}
            </p>
          </Show>
        </>
      )}
    </FormDialog>
  );
}
