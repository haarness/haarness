import { t, errorLine } from "./i18n";
import { For, Show, createSignal, onMount } from "solid-js";
import { createStore } from "solid-js/store";
import { Select } from "./Select";
import { Button, Icon, dismissOnBackdrop } from "./ui";
import { cancelled } from "./error-text";
import { keychainInput, newMcpDraft, renameKeychainReference, secretLooking, type McpPair } from "./mcp-form";

/** What this computer calls the store a secret is kept in. */
function keepLabel(platform: string) {
  return platform === "darwin"
    ? t("Store in Keychain")
    : platform === "win32"
      ? t("Store in Credential Manager")
      : t("Store in keyring");
}

export function McpForm(props: {
  save: (input: Record<string, unknown>) => Promise<unknown>;
  /** Keeps a secret in the system's keychain as a new entry, never replacing
   * one, and returns the entry's name (`name`, or `name-2` when that is taken). */
  storeSecret: (name: string, secret: string) => Promise<string>;
  /** Takes entries this form just stored out again. */
  removeSecrets: (names: string[]) => Promise<unknown>;
  /** The platform the server runs on, which names the keychain. */
  platform: string;
  close: () => void;
}) {
  const [draft, setDraft] = createStore(newMcpDraft());
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal("");
  let dialog!: HTMLDialogElement;
  let failure!: HTMLParagraphElement;
  const close = () => {
    if (!dialog.isConnected || !dialog.open) return;
    dialog.close();
    props.close();
  };
  onMount(() => dialog.showModal());
  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (saving()) return;
    setError("");
    // Entries stored for this server, taken out again if it is not added.
    const stored: string[] = [];
    try {
      const { input, secrets } = keychainInput(draft);
      setSaving(true);
      // The secrets first: settings that name a missing entry would fail.
      for (const { name, secret } of secrets) {
        const entry = await props.storeSecret(name, secret);
        stored.push(entry);
        if (entry !== name) renameKeychainReference(input, name, entry);
      }
      await props.save(input);
      setSaving(false);
      close();
    } catch (error) {
      if (stored.length) await props.removeSecrets(stored).catch(() => undefined);
      setSaving(false);
      if (cancelled(error)) return;
      setError(errorLine(error));
      failure?.focus();
    }
  }
  return (
    <dialog
      ref={dialog}
      class="dialog mcp-dialog"
      aria-label={t("Add MCP server")}
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
      {...dismissOnBackdrop(close)}
    >
      <form
        onSubmit={submit}
        onInput={() => setError("")}
        onChange={() => setError("")}
      >
        <header>
          <div>
            <h2>{t("Add MCP server")}</h2>
            <p class="muted">{t("Connect tools and resources to Haarness.")}</p>
          </div>
          <Button
            icon="close"
            label={t("Close dialog")}
            onClick={close}
          />
        </header>
        <fieldset disabled={saving()} class="mcp-fields">
          <div class="mcp-columns">
            <label class="field">
              <span>{t("Name")}</span>
              <input
                autofocus
                required
                value={draft.name}
                placeholder={t("e.g. project-tools")}
                onInput={(event) => setDraft("name", event.currentTarget.value)}
              />
            </label>
            <label class="field">
              <span>{t("Available in")}</span>
              <Select
                aria-label={t("Available in")}
                value={draft.scope}
                disabled={saving()}
                options={[
                  { value: "global", label: t("All projects") },
                  { value: "local", label: t("This project") },
                ]}
                onChange={(value) =>
                  setDraft("scope", value as "local" | "global")
                }
              />
            </label>
          </div>
          <div class="mcp-transport" role="group" aria-label={t("Server type")}>
            <button
              type="button"
              aria-pressed={draft.type === "local"}
              onClick={() => setDraft("type", "local")}
            >
              <Icon name="terminal" />
              <span>
                {t("Local command")}
                <small>{t("Run an executable on this computer")}</small>
              </span>
            </button>
            <button
              type="button"
              aria-pressed={draft.type === "remote"}
              onClick={() => setDraft("type", "remote")}
            >
              <Icon name="web" />
              <span>
                {t("Remote HTTP")}
                <small>{t("Connect to a server URL")}</small>
              </span>
            </button>
          </div>
          <Show
            when={draft.type === "local"}
            fallback={
              <>
                <label class="field">
                  <span>{t("Server URL")}</span>
                  <input
                    type="url"
                    required
                    value={draft.url}
                    placeholder="https://example.com/mcp"
                    onInput={(event) =>
                      setDraft("url", event.currentTarget.value)
                    }
                  />
                </label>
                <Pairs
                  kind="headers"
                  rows={draft.headers}
                  disabled={saving()}
                  keep={keepLabel(props.platform)}
                  add={() =>
                    setDraft("headers", (rows) => [
                      ...rows,
                      { key: "", value: "" },
                    ])
                  }
                  remove={(index) =>
                    setDraft("headers", (rows) =>
                      rows.filter((_, i) => i !== index),
                    )
                  }
                  change={(index, field, value) =>
                    setDraft("headers", index, field, value)
                  }
                  vault={(index, on) => setDraft("headers", index, "vault", on)}
                />
                <label class="field">
                  <span>{t("OAuth")}</span>
                  <Select
                    aria-label={t("OAuth")}
                    value={draft.oauth}
                    disabled={saving()}
                    options={[
                      { value: "auto", label: t("Auto-detect") },
                      { value: "enabled", label: t("Enabled") },
                      { value: "disabled", label: t("Disabled") },
                    ]}
                    onChange={(value) =>
                      setDraft(
                        "oauth",
                        value as "auto" | "enabled" | "disabled",
                      )
                    }
                  />
                  <small class="muted">
                    {draft.oauth === "auto"
                      ? t("Detect whether this server requires OAuth sign-in.")
                      : draft.oauth === "enabled"
                        ? t("Use Authorize after saving to sign in to this server.")
                        : t("Authenticate with request headers if needed.")}
                  </small>
                </label>
                <Show when={draft.oauth !== "disabled"}>
                  <details class="mcp-oauth">
                    <summary>
                      {t("OAuth options")}{" "}
                      <span class="muted">{t("Optional")}</span>
                    </summary>
                    <div class="mcp-columns">
                      <label class="field">
                        <span>{t("Client ID")}</span>
                        <input
                          value={draft.oauthClientId}
                          placeholder={t("Provided by your server")}
                          onInput={(event) =>
                            setDraft("oauthClientId", event.currentTarget.value)
                          }
                        />
                      </label>
                      <label class="field">
                        <span>{t("Scopes")}</span>
                        <input
                          aria-label={t("Scopes")}
                          value={draft.oauthScopes}
                          placeholder={t("e.g. read write")}
                          onInput={(event) =>
                            setDraft("oauthScopes", event.currentTarget.value)
                          }
                        />
                        <small class="muted">
                          {t("Separate scopes with spaces.")}
                        </small>
                      </label>
                    </div>
                  </details>
                </Show>
              </>
            }
          >
            <label class="field">
              <span>{t("Executable")}</span>
              <input
                required
                value={draft.command}
                placeholder={t("npx, uvx, or /path/to/server")}
                onInput={(event) =>
                  setDraft("command", event.currentTarget.value)
                }
              />
            </label>
            <section class="mcp-rows" aria-label={t("Arguments")}>
              <header>
                <h3>{t("Arguments")}</h3>
                <button
                  type="button"
                  onClick={() =>
                    setDraft("args", (rows) => [
                      ...rows,
                      { key: "", value: "" },
                    ])
                  }
                >
                  <Icon name="plus" size="sm" /> {t("Add argument")}
                </button>
              </header>
              <Show
                when={draft.args.length}
                fallback={
                  <p class="muted mcp-empty">
                    {t(
                      "No arguments. Add flags or positional arguments if the server needs them.",
                    )}
                  </p>
                }
              >
                <div class="mcp-row-labels mcp-argument-row" aria-hidden="true">
                  <span>{t("Argument or flag")}</span>
                  <span>{t("Value (optional)")}</span>
                </div>
              </Show>
              <For each={draft.args}>
                {(row, index) => (
                  <div class="mcp-argument-row" data-mcp-argument>
                    <input
                      aria-label={t("Argument {index}", { index: index() + 1 })}
                      value={row.key}
                      placeholder={t("-y, package-name, --port")}
                      onInput={(event) =>
                        setDraft(
                          "args",
                          index(),
                          "key",
                          event.currentTarget.value,
                        )
                      }
                    />
                    <input
                      aria-label={t("Argument {index} value", {
                        index: index() + 1,
                      })}
                      value={row.value}
                      placeholder={t("Value for this flag")}
                      onInput={(event) =>
                        setDraft(
                          "args",
                          index(),
                          "value",
                          event.currentTarget.value,
                        )
                      }
                    />
                    <div class="mcp-row-actions">
                      <Button
                        icon="send"
                        label={t("Move argument {index} up", {
                          index: index() + 1,
                        })}
                        disabled={saving() || index() === 0}
                        onClick={() =>
                          setDraft("args", (rows) => {
                            const next = [...rows];
                            [next[index() - 1], next[index()]] = [
                              next[index()],
                              next[index() - 1],
                            ];
                            return next;
                          })
                        }
                      />
                      <Button
                        icon="close"
                        label={t("Remove argument {index}", {
                          index: index() + 1,
                        })}
                        disabled={saving()}
                        onClick={() =>
                          setDraft("args", (rows) =>
                            rows.filter((_, i) => i !== index()),
                          )
                        }
                      />
                    </div>
                  </div>
                )}
              </For>
              <Show when={draft.args.length}>
                <p class="muted mcp-hint">
                  {t(
                    "Arguments run in this order. Spaces inside a field stay together; no shell quotes needed.",
                  )}
                </p>
              </Show>
            </section>
            <Pairs
              kind="environment"
              rows={draft.environment}
              disabled={saving()}
              keep={keepLabel(props.platform)}
              add={() =>
                setDraft("environment", (rows) => [
                  ...rows,
                  { key: "", value: "" },
                ])
              }
              remove={(index) =>
                setDraft("environment", (rows) =>
                  rows.filter((_, i) => i !== index),
                )
              }
              change={(index, field, value) =>
                setDraft("environment", index, field, value)
              }
              vault={(index, on) => setDraft("environment", index, "vault", on)}
            />
          </Show>
          <label class="mcp-enabled">
            <input
              type="checkbox"
              checked={draft.enabled}
              onChange={(event) =>
                setDraft("enabled", event.currentTarget.checked)
              }
            />
            <span>
              {t("Enable server")}
              <small class="muted">
                {t("Make its tools available after saving.")}
              </small>
            </span>
          </label>
        </fieldset>
        <Show when={error()}>
          <p ref={failure} role="alert" tabindex="-1" class="mcp-error">
            {error()}
          </p>
        </Show>
        <footer>
          <button type="button" onClick={close}>
            {t("Cancel")}
          </button>
          <button class="primary" type="submit" disabled={saving()}>
            {saving() ? t("Saving…") : t("Add server")}
          </button>
        </footer>
      </form>
    </dialog>
  );
}

const pairText = {
  headers: {
    title: () => t("Request headers"),
    add: () => t("Add header"),
    empty: () => t("No request headers added."),
    show: () => t("Show header values"),
    hide: () => t("Hide header values"),
    keyLabel: () => t("Header name"),
    keyName: (index: number) => t("Header name {index}", { index }),
    valueName: (index: number) => t("header {index} value", { index }),
    remove: (index: number) => t("Remove header {index}", { index }),
    keyPlaceholder: () => "Authorization",
    valuePlaceholder: () => "Bearer token",
  },
  environment: {
    title: () => t("Environment variables"),
    add: () => t("Add environment variable"),
    empty: () => t("No environment variables added."),
    show: () => t("Show environment variable values"),
    hide: () => t("Hide environment variable values"),
    keyLabel: () => t("Variable name"),
    keyName: (index: number) => t("Variable name {index}", { index }),
    valueName: (index: number) =>
      t("environment variable {index} value", { index }),
    remove: (index: number) => t("Remove environment variable {index}", { index }),
    keyPlaceholder: () => "API_KEY",
    valuePlaceholder: () => t("Value"),
  },
};

function Pairs(props: {
  kind: keyof typeof pairText;
  rows: McpPair[];
  disabled: boolean;
  add: () => void;
  remove: (index: number) => void;
  change: (index: number, field: "key" | "value", value: string) => void;
  /** The label of the keep-in-keychain toggle a secret-looking value offers. */
  keep: string;
  vault: (index: number, on: boolean) => void;
}) {
  const [visible, setVisible] = createSignal(false);
  const text = () => pairText[props.kind];
  return (
    <section class="mcp-rows" aria-label={text().title()}>
      <header>
        <h3>{text().title()}</h3>
        <div class="mcp-row-actions">
          <Show when={props.rows.length}>
            <Button
              icon={visible() ? "eye-off" : "eye"}
              label={visible() ? text().hide() : text().show()}
              disabled={props.disabled}
              onClick={() => setVisible(!visible())}
            />
          </Show>
          <button type="button" onClick={props.add}>
            <Icon name="plus" size="sm" /> {text().add()}
          </button>
        </div>
      </header>
      <Show
        when={props.rows.length}
        fallback={<p class="muted mcp-empty">{text().empty()}</p>}
      >
        <div class="mcp-row-labels mcp-pair-row" aria-hidden="true">
          <span>{text().keyLabel()}</span>
          <span>{t("Value")}</span>
        </div>
      </Show>
      <For each={props.rows}>
        {(row, index) => (
          <div class="mcp-pair-row" data-mcp-pair>
            <input
              aria-label={text().keyName(index() + 1)}
              value={row.key}
              placeholder={text().keyPlaceholder()}
              spellcheck={false}
              onInput={(event) =>
                props.change(index(), "key", event.currentTarget.value)
              }
            />
            <input
              aria-label={text().valueName(index() + 1)}
              type={visible() ? "text" : "password"}
              autocomplete="off"
              spellcheck={false}
              value={row.value}
              placeholder={text().valuePlaceholder()}
              onInput={(event) =>
                props.change(index(), "value", event.currentTarget.value)
              }
            />
            <div class="mcp-row-actions">
              {/* Offered while the value looks like a secret; the column
                  stays reserved so typing never moves the fields. */}
              <Show
                when={secretLooking(row) || row.vault}
                fallback={<span class="icon-button-space" aria-hidden="true" />}
              >
                <Button
                  icon="key"
                  class="mcp-keep"
                  label={`${props.keep}: ${row.key || text().keyLabel()}`}
                  hint={t("Saved there when you add the server; the settings keep only a reference.")}
                  active={!!row.vault}
                  disabled={props.disabled}
                  onClick={() => props.vault(index(), !row.vault)}
                />
              </Show>
              <Button
                icon="close"
                label={text().remove(index() + 1)}
                disabled={props.disabled}
                onClick={() => props.remove(index())}
              />
            </div>
          </div>
        )}
      </For>
    </section>
  );
}
