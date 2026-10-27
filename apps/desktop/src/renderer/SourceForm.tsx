import { t, errorLine } from "./i18n";
import { cancelled } from "./error-text";
import { Show, createSignal } from "solid-js";
import { createStore } from "solid-js/store";
import { FormDialog } from "./FormDialog";
import { Select } from "./Select";

export function SourceForm(props: {
  kind: "plugin" | "marketplace" | "extension";
  close: () => void;
  save: (source: string) => Promise<unknown>;
}) {
  const [form, setForm] = createStore({
    type: "local",
    local: "",
    repository: "",
    package: "",
    version: "",
    plugin: "",
    marketplace: "",
  });
  const [pickerError, setPickerError] = createSignal("");
  async function browse(type: "directory" | "file") {
    setPickerError("");
    try {
      const path = await window.haarness.pick(type);
      if (path) setForm("local", path);
    } catch (error) {
      if (!cancelled(error)) setPickerError(errorLine(error));
    }
  }
  const extension = props.kind === "extension";
  const types = () => [
    {
      value: "local",
      label: extension ? t("Local file or folder") : t("Local folder"),
    },
    ...(extension
      ? [{ value: "package", label: t("npm package") }]
      : [{ value: "repository", label: t("Git repository") }]),
    ...(props.kind === "plugin"
      ? [{ value: "marketplace", label: t("Registered marketplace") }]
      : []),
  ];
  async function save() {
    let source = "";
    if (form.type === "local") source = form.local.trim();
    if (form.type === "repository") source = form.repository.trim();
    if (form.type === "package") {
      if (!/^(@[a-z0-9._-]+\/)?[a-z0-9._-]+$/i.test(form.package.trim()))
        throw Error(t("Enter an npm package name, such as @team/extension."));
      if (form.version && /[\s:/]/.test(form.version))
        throw Error(
          t("Enter a package version or tag, such as 1.0.0 or latest."),
        );
      source =
        "npm:" +
        form.package.trim() +
        (form.version.trim() ? "@" + form.version.trim() : "");
    }
    if (form.type === "marketplace") {
      if (!form.plugin.trim() || !form.marketplace.trim())
        throw Error(t("Enter a plugin and marketplace name."));
      source = form.plugin.trim() + "@" + form.marketplace.trim();
    }
    if (!source) throw Error(t("Choose a source to continue."));
    await props.save(source);
  }
  return (
    <FormDialog
      title={
        props.kind === "marketplace"
          ? t("Register a marketplace")
          : extension
            ? t("Install an extension")
            : t("Install a plugin")
      }
      description={
        extension
          ? t(
              "Extensions run trusted JavaScript or TypeScript with access to your files and network.",
            )
          : props.kind === "marketplace"
            ? t("Choose the folder or Git repository that hosts the marketplace. Its plugins can then be installed from Registered marketplace.")
            : t("Choose where to install from.")
      }
      submit={
        props.kind === "marketplace"
          ? t("Register")
          : extension
            ? t("Trust & install")
            : t("Install")
      }
      close={props.close}
      save={save}
    >
      {(busy) => (
        <>
          <Show when={pickerError()}>
            <p class="form-error" role="alert">
              {pickerError()}
            </p>
          </Show>
          <label class="field">
            <span>{t("Source")}</span>
            <Select
              aria-label={t("Source")}
              value={form.type}
              options={types()}
              disabled={busy()}
              onChange={(value) => setForm("type", value)}
            />
          </label>
          <Show when={form.type === "local"}>
            <label class="field">
              <span>{extension ? t("Extension path") : t("Folder")}</span>
              <input
                required
                value={form.local}
                placeholder={t("Choose a local source")}
                onInput={(e) => setForm("local", e.currentTarget.value)}
              />
            </label>
            <div class="form-hint-row">
              <button type="button" onClick={() => browse("directory")}>
                {t("Browse folder")}
              </button>
              <Show when={extension}>
                <button type="button" onClick={() => browse("file")}>
                  {t("Browse file")}
                </button>
              </Show>
            </div>
          </Show>
          <Show when={form.type === "repository"}>
            <label class="field">
              <span>{t("Repository URL")}</span>
              <input
                required
                value={form.repository}
                placeholder="https://github.com/team/repository.git"
                onInput={(e) => setForm("repository", e.currentTarget.value)}
              />
            </label>
          </Show>
          <Show when={form.type === "package"}>
            <div class="form-grid">
              <label class="field">
                <span>{t("Package name")}</span>
                <input
                  required
                  value={form.package}
                  placeholder="@team/extension"
                  onInput={(e) => setForm("package", e.currentTarget.value)}
                />
              </label>
              <label class="field">
                <span>{t("Version (optional)")}</span>
                <input
                  value={form.version}
                  placeholder="latest"
                  onInput={(e) => setForm("version", e.currentTarget.value)}
                />
              </label>
            </div>
          </Show>
          <Show when={form.type === "marketplace"}>
            <div class="form-grid">
              <label class="field">
                <span>{t("Plugin name")}</span>
                <input
                  required
                  value={form.plugin}
                  onInput={(e) => setForm("plugin", e.currentTarget.value)}
                />
              </label>
              <label class="field">
                <span>{t("Marketplace name")}</span>
                <input
                  required
                  value={form.marketplace}
                  onInput={(e) => setForm("marketplace", e.currentTarget.value)}
                />
              </label>
            </div>
          </Show>
        </>
      )}
    </FormDialog>
  );
}
