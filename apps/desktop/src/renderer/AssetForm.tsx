import { t } from "./i18n";
import { For, Show, createSignal, onMount } from "solid-js";
import { createStore } from "solid-js/store";
import type { ToolCatalogResponse } from "@haarness/sdk";
import { useApp } from "./state";
import { Select } from "./Select";
import { FormDialog } from "./FormDialog";
import { ExecutionFields } from "./ExecutionFields";
import { Button } from "./ui";

export type AssetKind = "agent" | "command" | "skill";
export interface AssetRow {
  name: string;
  description?: string;
  scope?: string;
  managed?: boolean;
  native?: boolean;
  readOnly?: boolean;
  customized?: boolean;
}

export function AssetForm(props: {
  kind: AssetKind;
  prior: Record<string, unknown>;
  editing: boolean;
  close: () => void;
  saved: () => void;
}) {
  const app = useApp();
  const body =
    props.kind === "agent"
      ? "prompt"
      : props.kind === "skill"
        ? "instructions"
        : "template";
  const customizing = !!props.prior.readOnly;
  const heading = () =>
    props.kind === "agent"
      ? props.editing
        ? t("Edit agent")
        : t("Create agent")
      : props.kind === "command"
        ? props.editing
          ? t("Edit command")
          : t("Create command")
        : props.editing
          ? t("Edit skill")
          : t("Create skill");
  const [form, setForm] = createStore({
    name: String(props.prior.name ?? ""),
    description: String(props.prior.description ?? ""),
    body: String(props.prior[body] ?? props.prior.content ?? ""),
    scope: props.prior.scope === "local" ? "local" : "global",
    mode: String(props.prior.mode ?? "all"),
    model: String(props.prior.model ?? ""),
    agent: String(props.prior.agent ?? ""),
    argumentHint: String(props.prior.argumentHint ?? ""),
    notes: String(props.prior.notes ?? ""),
    tools: Object.entries(
      (props.prior.tools as Record<string, boolean>) ?? {},
    ).map(([name, enabled]) => ({ name, enabled })),
  });
  const [tools, setTools] = createSignal<ToolCatalogResponse["tools"]>([]);
  onMount(() => {
    if (props.kind === "agent")
      void app
        .api<ToolCatalogResponse>("toolCatalog")
        .then((data) => setTools(data.tools))
        .catch(() => {});
  });
  async function save() {
    if (!form.name.trim() || !form.description.trim() || !form.body.trim())
      throw Error(t("Add a name, description, and instructions."));
    const names = form.tools.map((tool) => tool.name);
    if (new Set(names).size !== names.length || names.some((name) => !name))
      throw Error(t("Choose a different tool for each override."));
    await app.native({
      action: "asset",
      kind: props.kind,
      input: {
        action: customizing ? "customize" : props.editing ? "edit" : "create",
        name: form.name.trim(),
        scope: form.scope,
        description: form.description,
        [body]: form.body,
        ...(props.kind === "agent"
          ? {
              mode: form.mode,
              model: form.model || null,
              tools: Object.fromEntries(
                form.tools.map((tool) => [tool.name, tool.enabled]),
              ),
            }
          : {}),
        ...(props.kind === "command"
          ? {
              agent: form.agent || null,
              model: form.model || null,
              argumentHint: form.argumentHint || null,
            }
          : {}),
        ...(props.kind === "skill" ? { notes: form.notes || null } : {}),
      },
    });
    props.saved();
  }
  return (
    <FormDialog
      title={heading()}
      description={
        customizing
          ? t("Save your own version. You can reset it to the original later.")
          : props.kind === "command"
            ? t("Create a reusable prompt you can run from the conversation.")
            : props.kind === "agent"
              ? t("Define how this agent works and which tools it can use.")
              : t("Give Haarness reusable instructions for a specific task.")
      }
      submit={props.editing ? t("Save changes") : t("Create")}
      close={props.close}
      save={save}
    >
      {(busy) => (
        <>
          <div class="form-grid">
            <label class="field">
              <span>{t("Name")}</span>
              <input
                autofocus
                required
                value={form.name}
                readOnly={props.editing}
                placeholder={
                  props.kind === "command"
                    ? "review-changes"
                    : props.kind === "agent"
                      ? "code-reviewer"
                      : "release-checklist"
                }
                onInput={(e) => setForm("name", e.currentTarget.value)}
              />
            </label>
            <label class="field">
              <span>{t("Available in")}</span>
              <Select
                aria-label={t("Available in")}
                value={form.scope}
                disabled={busy() || (props.editing && !customizing)}
                options={[
                  { value: "global", label: t("All projects") },
                  { value: "local", label: t("This project") },
                ]}
                onChange={(value) => setForm("scope", value)}
              />
              <Show when={props.editing && !customizing}>
                <small class="muted">
                  {t("Edits stay in the original location.")}
                </small>
              </Show>
            </label>
          </div>
          <label class="field">
            <span>{t("Description")}</span>
            <input
              required
              value={form.description}
              placeholder={t("When should this be used?")}
              onInput={(e) => setForm("description", e.currentTarget.value)}
            />
          </label>
          <label class="field">
            <span>
              {props.kind === "command"
                ? t("Prompt template")
                : t("Instructions")}
            </span>
            <textarea
              class="instruction-editor"
              aria-label={
                props.kind === "command"
                  ? t("Prompt template")
                  : t("Instructions")
              }
              required
              value={form.body}
              placeholder={
                props.kind === "command"
                  ? t("Review $ARGUMENTS and suggest concrete improvements.")
                  : t("Describe the task, approach, and expected result…")
              }
              onInput={(e) => setForm("body", e.currentTarget.value)}
            />
          </label>
          <Show when={props.kind === "command"}>
            <div class="form-hint-row">
              <small class="muted">
                {t("Use the text entered after /{name} in this prompt.", {
                  name: form.name || "command",
                })}
              </small>
              <button
                type="button"
                onClick={() =>
                  setForm(
                    "body",
                    (body) =>
                      body +
                      (body && !/\s$/.test(body) ? " " : "") +
                      "$ARGUMENTS",
                  )
                }
              >
                {t("Insert arguments")}
              </button>
            </div>
          </Show>
          <Show when={props.kind !== "skill"}>
            <Show when={props.kind === "agent"}>
              <label class="field">
                <span>{t("Available as")}</span>
                <Select
                  aria-label={t("Available as")}
                  value={form.mode}
                  disabled={busy()}
                  options={[
                    { value: "all", label: t("Session agent and subagent") },
                    { value: "primary", label: t("Session agent") },
                    { value: "subagent", label: t("Subagent only") },
                  ]}
                  onChange={(value) => setForm("mode", value)}
                />
              </label>
            </Show>
            <ExecutionFields
              model={form.model}
              agent={form.agent}
              disabled={busy()}
              setModel={(value) => setForm("model", value)}
              setAgent={
                props.kind === "command"
                  ? (value) => setForm("agent", value)
                  : undefined
              }
            />
          </Show>
          <Show when={props.kind === "command"}>
            <label class="field">
              <span>{t("Argument hint")}</span>
              <input
                aria-label={t("Argument hint")}
                value={form.argumentHint}
                placeholder={t("e.g. file or pull request")}
                onInput={(e) => setForm("argumentHint", e.currentTarget.value)}
              />
              <small class="muted">
                {t("Shown next to the command in the slash menu.")}
              </small>
            </label>
          </Show>
          <Show when={props.kind === "skill"}>
            <label class="field">
              <span>{t("Notes (optional)")}</span>
              <textarea
                value={form.notes}
                placeholder={t("Setup requirements or additional context")}
                onInput={(e) => setForm("notes", e.currentTarget.value)}
              />
            </label>
          </Show>
          <Show when={props.kind === "agent"}>
            <details class="form-advanced">
              <summary>
                {t("Tool access")}{" "}
                <span class="muted">
                  {form.tools.length
                    ? t("{count} overrides", { count: form.tools.length })
                    : t("Use agent defaults")}
                </span>
              </summary>
              <p class="muted">
                {t("Overrides apply within the session’s permissions.")}
              </p>
              <For each={form.tools}>
                {(tool, index) => (
                  <div class="form-tool-row">
                    <Select
                      aria-label={t("Tool {index}", { index: index() + 1 })}
                      value={tool.name}
                      disabled={busy()}
                      options={[
                        { value: "", label: t("Choose a tool") },
                        ...tools()
                          .filter(
                            (entry) =>
                              entry.id === tool.name ||
                              !form.tools.some((row) => row.name === entry.id),
                          )
                          .map((entry) => ({
                            value: entry.id,
                            label: entry.id,
                            group: entry.groupTitle,
                          })),
                        ...(tool.name &&
                        !tools().some((entry) => entry.id === tool.name)
                          ? [{ value: tool.name, label: tool.name }]
                          : []),
                      ]}
                      onChange={(value) =>
                        setForm("tools", index(), "name", value)
                      }
                    />
                    <Select
                      aria-label={t("Tool {index} access", {
                        index: index() + 1,
                      })}
                      value={tool.enabled ? "enabled" : "disabled"}
                      disabled={busy()}
                      options={[
                        { value: "enabled", label: t("Enabled") },
                        { value: "disabled", label: t("Disabled") },
                      ]}
                      onChange={(value) =>
                        setForm(
                          "tools",
                          index(),
                          "enabled",
                          value === "enabled",
                        )
                      }
                    />
                    <Button
                      icon="close"
                      label={t("Remove tool override {index}", {
                        index: index() + 1,
                      })}
                      disabled={busy()}
                      onClick={() =>
                        setForm("tools", (rows) =>
                          rows.filter((_, i) => i !== index()),
                        )
                      }
                    />
                  </div>
                )}
              </For>
              <button
                type="button"
                onClick={() =>
                  setForm("tools", (rows) => [
                    ...rows,
                    { name: "", enabled: false },
                  ])
                }
              >
                {t("Add tool override")}
              </button>
            </details>
          </Show>
        </>
      )}
    </FormDialog>
  );
}
