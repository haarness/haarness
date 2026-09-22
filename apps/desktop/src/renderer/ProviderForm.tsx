import { t } from "./i18n";
import { For, Show, createSignal } from "solid-js";
import { createStore } from "solid-js/store";
import { FormDialog } from "./FormDialog";
import { Select } from "./Select";
import { Button } from "./ui";
import { useApp } from "./state";

const protocols = [
  ["openai-chat-completions", "OpenAI Chat Completions"],
  ["openai-responses", "OpenAI Responses"],
  ["anthropic", "Anthropic Messages"],
  ["google-generative-ai", "Google Gemini"],
  ["azure-openai", "Azure OpenAI"],
  ["google-vertex", "Google Vertex AI"],
  ["amazon-bedrock", "Amazon Bedrock"],
].map(([value, label]) => ({ value, label }));

export function ProviderForm(props: { close: () => void }) {
  const app = useApp();
  const [form, setForm] = createStore({
    id: "",
    name: "",
    url: "",
    protocol: protocols[0].value,
    auth: "key",
    key: "",
    env: "",
    scope: "global",
    discovery: "dynamic",
    models: [] as { value: string }[],
  });
  const [created, setCreated] = createSignal(false);
  async function save() {
    const id = form.id.trim();
    if (!/^[a-z0-9_-]+$/.test(id))
      throw Error(
        t(
          "Use lowercase letters, numbers, hyphens, or underscores for the provider ID.",
        ),
      );
    if (
      !created() &&
      app.state.providers.some((provider) => provider.id === id)
    )
      throw Error(
        t("This provider ID is already in use. Choose a different ID."),
      );
    let url: URL;
    try {
      url = new URL(form.url.trim());
    } catch {
      throw Error(t("Enter a valid provider URL."));
    }
    if (
      !["https:", "http:"].includes(url.protocol) ||
      url.username ||
      url.password
    )
      throw Error(t("Use an HTTP or HTTPS URL without embedded credentials."));
    if (
      form.auth === "environment" &&
      !/^[A-Za-z_][A-Za-z0-9_]*$/.test(form.env.trim())
    )
      throw Error(t("Enter a valid environment-variable name."));
    const models = form.models.map((row) => row.value.trim()).filter(Boolean);
    if (
      form.discovery === "manual" &&
      (!models.length || new Set(models).size !== models.length)
    )
      throw Error(t("Add at least one model ID, with no duplicates."));
    if (!created()) {
      const args = [
        "provider",
        "add",
        id,
        "--url",
        form.url.trim(),
        "--protocol",
        form.protocol,
      ];
      if (form.name.trim()) args.push("--name", form.name.trim());
      if (form.scope === "local") args.push("--project");
      if (form.auth === "none") args.push("--no-auth");
      if (form.auth === "environment")
        args.push("--api-key-env", form.env.trim());
      if (form.discovery === "manual")
        for (const model of models) args.push("--model", model);
      await window.haarness.command(
        app.state.connection!.id,
        args,
        crypto.randomUUID(),
      );
      setCreated(true);
    }
    if (form.auth === "key" && form.key)
      await app.native({
        action: "set_credential",
        provider: id,
        key: form.key,
      });
    await app.catalogs(true);
  }
  return (
    <FormDialog
      title={t("Custom provider")}
      description={t(
        "Connect a compatible API and choose how its models are discovered.",
      )}
      submit={t("Add provider")}
      close={props.close}
      save={save}
    >
      {(busy) => (
        <>
          <div class="form-grid">
            <label class="field">
              <span>{t("Provider ID")}</span>
              <input
                autofocus
                required
                readOnly={created()}
                value={form.id}
                placeholder="my-provider"
                onInput={(e) => setForm("id", e.currentTarget.value)}
              />
            </label>
            <label class="field">
              <span>{t("Display name")}</span>
              <input
                readOnly={created()}
                value={form.name}
                placeholder={t("My provider")}
                onInput={(e) => setForm("name", e.currentTarget.value)}
              />
            </label>
          </div>
          <div class="form-grid">
            <label class="field">
              <span>{t("API format")}</span>
              <Select
                aria-label={t("API format")}
                disabled={busy() || created()}
                value={form.protocol}
                options={protocols.map((protocol) => ({
                  ...protocol,
                  label: t(protocol.label),
                }))}
                onChange={(value) => setForm("protocol", value)}
              />
            </label>
            <label class="field">
              <span>{t("Available in")}</span>
              <Select
                aria-label={t("Available in")}
                value={form.scope}
                disabled={busy() || created()}
                options={[
                  { value: "global", label: t("All projects") },
                  { value: "local", label: t("This project") },
                ]}
                onChange={(value) => setForm("scope", value)}
              />
            </label>
          </div>
          <label class="field">
            <span>{t("Base URL")}</span>
            <input
              readOnly={created()}
              type="url"
              required
              value={form.url}
              placeholder="https://api.example.com/v1"
              onInput={(e) => setForm("url", e.currentTarget.value)}
            />
          </label>
          <label class="field">
            <span>{t("Authentication")}</span>
            <Select
              aria-label={t("Authentication")}
              disabled={busy() || created()}
              value={form.auth}
              options={[
                { value: "key", label: t("API key") },
                { value: "environment", label: t("Environment variable") },
                { value: "none", label: t("No authentication") },
              ]}
              onChange={(value) => setForm("auth", value)}
            />
          </label>
          <Show when={form.auth === "key"}>
            <label class="field">
              <span>{t("API key")}</span>
              <input
                type="password"
                autocomplete="off"
                value={form.key}
                placeholder={t("Optional — connect later")}
                onInput={(e) => setForm("key", e.currentTarget.value)}
              />
            </label>
          </Show>
          <Show when={form.auth === "environment"}>
            <label class="field">
              <span>{t("Environment variable")}</span>
              <input
                readOnly={created()}
                required
                value={form.env}
                placeholder="MY_PROVIDER_API_KEY"
                onInput={(e) => setForm("env", e.currentTarget.value)}
              />
            </label>
          </Show>
          <label class="field">
            <span>{t("Models")}</span>
            <Select
              aria-label={t("Models")}
              disabled={busy() || created()}
              value={form.discovery}
              options={[
                { value: "dynamic", label: t("Discover from provider") },
                { value: "manual", label: t("Choose model IDs") },
              ]}
              onChange={(value) => setForm("discovery", value)}
            />
            <small class="muted">
              {form.discovery === "dynamic"
                ? t("Load the provider’s current model list when supported.")
                : t("Only these model IDs will be available.")}
            </small>
          </label>
          <Show when={form.discovery === "manual"}>
            <For each={form.models}>
              {(row, index) => (
                <div class="form-list-row">
                  <input
                    readOnly={created()}
                    aria-label={t("Model ID {index}", { index: index() + 1 })}
                    value={row.value}
                    placeholder={t("Model ID")}
                    onInput={(e) =>
                      setForm("models", index(), "value", e.currentTarget.value)
                    }
                  />
                  <Button
                    icon="close"
                    label={t("Remove model {index}", { index: index() + 1 })}
                    disabled={busy() || created()}
                    onClick={() =>
                      setForm("models", (rows) =>
                        rows.filter((_, i) => i !== index()),
                      )
                    }
                  />
                </div>
              )}
            </For>
            <button
              type="button"
              disabled={created()}
              onClick={() =>
                setForm("models", (rows) => [...rows, { value: "" }])
              }
            >
              {t("Add model")}
            </button>
          </Show>
        </>
      )}
    </FormDialog>
  );
}
