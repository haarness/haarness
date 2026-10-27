import { language, t, errorLine } from "./i18n";
import { For, Show, createMemo, createSignal, onMount } from "solid-js";
import { createStore } from "solid-js/store";
import type { ToolCatalogResponse } from "@haarness/sdk";
import { useApp } from "./state";
import { FormDialog } from "./FormDialog";
import { Select } from "./Select";
import { Combobox, type ComboboxOption } from "./Combobox";
import { Icon } from "./icons";
import {
  lastAlternative,
  matcherKind,
  neverRuns,
  permissionNames,
  valueLabel,
  type MatcherTarget,
} from "./hook-matcher";
import { confirm, Empty } from "./ui";
import { cancelled } from "./error-text";
import { BaseTag } from "./SettingRow";
import "./management.css";

interface Hook {
  id: string;
  revision: string;
  event: string;
  matcher: string;
  /** Its matcher matches nothing its event sends, so it never runs. */
  neverRuns?: boolean;
  groupDisabled: boolean;
  handler: {
    command: string;
    args?: string[];
    shell?: string;
    timeout?: number;
    async?: boolean;
    disabled?: boolean;
  };
  source: {
    path: string;
    editable: boolean;
    scope: string;
    trusted: boolean;
  };
}
interface Catalog {
  items: Hook[];
  events: string[];
  /** What each event's matcher is compared with. */
  matchers?: Record<string, MatcherTarget>;
  errors: string[];
  projectTrusted: boolean;
}
/** Why a hook for an event compared with `target` never runs. */
function neverRunsText(target: MatcherTarget | undefined) {
  const values = new Intl.ListFormat(language(), { type: "disjunction" }).format(target?.values ?? []);
  return t("Never runs: this event only matches {values}.", { values });
}
export function Hooks() {
  const app = useApp();
  const connection = app.state.connection!;
  const request = <T,>(input: Record<string, unknown>) =>
    window.haarness.native<T>(connection.id, {
      action: "asset",
      kind: "hooks",
      input,
    });
  const [catalog, setCatalog] = createSignal<Catalog>();
  const [error, setError] = createSignal("");
  const [working, setBusy] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const busy = () => working() || loading();
  const [editor, setEditor] = createSignal<{ item?: Hook }>();
  async function load() {
    setLoading(true);
    setError("");
    try {
      setCatalog(await request<Catalog>({ action: "list" }));
    } catch (error) {
      if (!cancelled(error)) setError(errorLine(error));
    } finally {
      setLoading(false);
    }
  }
  async function mutate(input: Record<string, unknown>) {
    if (busy()) return;
    setBusy(true);
    setError("");
    try {
      await request(input);
      await load();
    } catch (error) {
      if (!cancelled(error)) setError(errorLine(error));
    } finally {
      setBusy(false);
    }
  }
  onMount(() => {
    if (!connection.remote) void load();
  });
  return (
    <div class="management-panel">
      <p class="muted">
        {t(
          "Run commands at key points in a chat. Changes apply when the runtime is idle.",
        )}
      </p>
      <Show
        when={!connection.remote}
        fallback={
          <p class="notice-inline">
            {t(
              "Manage hooks on the remote host. Desktop configuration editing is available for local projects.",
            )}
          </p>
        }
      >
        <div class="management-actions" data-setting="Create hook">
          <button
            class="primary"
            disabled={busy() || !catalog()}
            onClick={() => setEditor({})}
          >
            {t("Add hook")}
          </button>
          <button disabled={busy()} onClick={() => void load()}>
            {t("Refresh hooks")}
          </button>
        </div>
        <Show when={error()}>
          <p role="alert" class="form-error">
            {error()}
          </p>
        </Show>
        <For each={catalog()?.errors}>
          {(message) => (
            <p role="alert" class="form-error">
              {message}
            </p>
          )}
        </For>
        <Show
          when={catalog()}
          fallback={<p class="muted">{t("Loading hooks…")}</p>}
        >
          <Show
            when={catalog()!.projectTrusted}
            fallback={
              <p class="notice-inline">
                {t("Project configuration is off for this folder, so its hooks don’t run. Global and installed plugin hooks still run.")}
                {/* Desktop turns it off for a folder a link opened (or for every
                    folder with HAARNESS_DISABLE_PROJECT_CONFIG); Open project turns the first back on. */}
                {" "}
                {t("If a link opened this folder, open it with Open project to turn it on.")}
              </p>
            }
          >
            <span />
          </Show>
          <Show
            when={catalog()!.items.length}
            fallback={
              <Empty
                title={t("No hooks yet")}
                detail={t("Hooks run a command when something happens, like before a tool runs or when a reply finishes.")}
              />
            }
          >
            <For each={catalog()!.items}>
              {(hook) => (
                <article class="management-card" data-setting={"hooks:" + hook.id}>
                  <div class="management-card-heading">
                    <strong>{hook.event}</strong>
                    <span class="muted">{hook.matcher}</span>
                    <Show when={hook.neverRuns}>
                      <span
                        class="hook-never-tag"
                        title={neverRunsText(catalog()?.matchers?.[hook.event])}
                      >
                        {t("Never runs")}
                      </span>
                    </Show>
                    <label class="management-toggle">
                      <span>{t("Enabled")}</span>
                      <input
                        type="checkbox"
                        role="switch"
                        aria-label={t("Enable {event} hook {command}", {
                          event: hook.event,
                          command: hook.handler.command,
                        })}
                        checked={
                          !hook.handler.disabled && !hook.groupDisabled
                        }
                        disabled={
                          busy() ||
                          !hook.source.editable ||
                          hook.groupDisabled
                        }
                        onChange={(e) =>
                          void mutate({
                            action: "toggle",
                            id: hook.id,
                            revision: hook.revision,
                            enabled: e.currentTarget.checked,
                          })
                        }
                      />
                    </label>
                  </div>
                  <code class="management-command">
                    {hook.handler.command}
                  </code>
                  <small class="muted management-source">
                    <Show
                      when={hook.source.scope === "base"}
                      fallback={
                        hook.source.scope === "local"
                          ? t("This project")
                          : hook.source.scope === "global"
                            ? t("All projects")
                            : t("Plugin (read-only)")
                      }
                    >
                      <BaseTag />
                    </Show>{" "}
                    · {hook.source.path}
                  </small>
                  <Show when={!hook.source.trusted}>
                    <small class="muted">
                      {t("Inactive: project configuration is not trusted.")}
                    </small>
                  </Show>
                  <Show when={hook.groupDisabled}>
                    <small class="muted">
                      {t(
                        "The containing hook group is disabled in its configuration file.",
                      )}
                    </small>
                  </Show>
                  <div class="management-actions">
                    <button
                      disabled={busy()}
                      onClick={() => setEditor({ item: hook })}
                    >
                      {hook.source.editable ? t("Edit hook") : t("View hook")}
                    </button>
                    <Show when={hook.source.editable}>
                      <button
                        disabled={busy()}
                        onClick={async () => {
                          if (
                            await confirm(
                              t("Remove hook?"),
                              hook.handler.command,
                              { submit: t("Remove hook"), danger: true },
                            )
                          )
                            await mutate({
                              action: "delete",
                              id: hook.id,
                              revision: hook.revision,
                            });
                        }}
                      >
                        {t("Remove hook")}
                      </button>
                    </Show>
                  </div>
                </article>
              )}
            </For>
          </Show>
        </Show>
        <Show when={editor()} keyed>
          {(value) => (
            <HookForm
              item={value.item}
              events={catalog()?.events ?? []}
              matchers={catalog()?.matchers ?? {}}
              close={() => setEditor(undefined)}
              save={async (input) => {
                await request(input);
                await load();
              }}
            />
          )}
        </Show>
      </Show>
    </div>
  );
}
function HookForm(props: {
  item?: Hook;
  events: string[];
  matchers: Record<string, MatcherTarget>;
  close: () => void;
  save: (input: Record<string, unknown>) => Promise<void>;
}) {
  const app = useApp();
  const item = props.item;
  const readonly = !!item && !item.source.editable;
  // Tool names to suggest, from the tools this server offers, loaded when
  // the form opens; without them the field still takes any name.
  const [tools, setTools] = createSignal<ToolCatalogResponse["tools"]>([]);
  onMount(() => {
    if (!readonly)
      void app
        .api<ToolCatalogResponse>("toolCatalog")
        .then((data) => setTools(data.tools))
        .catch(() => {});
  });
  const [form, setForm] = createStore({
    event: item?.event ?? "PreToolUse",
    scope: "local",
    matcher: item?.matcher ?? "*",
    command: item?.handler.command ?? "",
    timeout: String(item?.handler.timeout ?? 60),
    args: item?.handler.args?.join("\n") ?? "",
    shell: item?.handler.shell ?? "",
    async: item?.handler.async ?? false,
  });
  const target = (): MatcherTarget | undefined => props.matchers[form.event];
  const compares = () => matcherKind(target());
  const matcherSet = () => !["", "*"].includes(form.matcher.trim());
  // An event with nothing to match asks for no matcher; one already written
  // stays visible, so it can be seen to be ignored and cleared.
  const showMatcher = () => compares() !== "nothing" || matcherSet();
  const matcherOptions = createMemo<ComboboxOption[]>(() => {
    const every = { value: "*", label: t("Runs every time") };
    switch (compares()) {
      case "tool":
        return [every, ...tools().map((tool) => ({ value: tool.id, label: tool.groupTitle }))];
      case "permission":
        return [every, ...permissionNames(tools().map((tool) => tool.id)).map((value) => ({ value }))];
      case "value":
        return [
          every,
          ...(target()?.values ?? []).map((value) => {
            const label = valueLabel(value);
            return { value, label: label && t(label) };
          }),
        ];
      default:
        return [];
    }
  });
  return (
    <FormDialog
      title={readonly ? t("View hook") : item ? t("Edit hook") : t("Add hook")}
      submit={readonly ? undefined : t("Save hook")}
      close={props.close}
      save={async () => {
        const timeout = Number(form.timeout);
        if (!form.command.trim()) throw Error(t("Enter a command."));
        if (!Number.isInteger(timeout) || timeout < 1 || timeout > 600)
          throw Error(t("Timeout must be between 1 and 600 seconds."));
        await props.save({
          action: item ? "update" : "create",
          id: item?.id,
          revision: item?.revision,
          ...form,
          timeout,
          args: form.args ? form.args.split("\n") : [],
        });
      }}
    >
      {(busy) => (
        <>
          <fieldset disabled={readonly || busy()}>
            <div class="form-grid">
              <label class="field">
                <span>{t("Event")}</span>
                <Select
                  aria-label={t("Hook event")}
                  options={props.events.map((value) => ({
                    value,
                    label: value,
                  }))}
                  value={form.event}
                  onChange={(value) => setForm("event", value)}
                  disabled={readonly || busy()}
                />
              </label>
              <Show when={!item}>
                <label class="field">
                  <span>{t("Scope")}</span>
                  <Select
                    aria-label={t("Hook scope")}
                    value={form.scope}
                    options={[
                      { value: "local", label: t("This project") },
                      { value: "global", label: t("All projects") },
                    ]}
                    onChange={(value) => setForm("scope", value)}
                    disabled={busy()}
                  />
                </label>
              </Show>
            </div>
            <Show when={showMatcher()}>
              <label class="field">
                <span>
                  {compares() === "tool"
                    ? t("Tools")
                    : compares() === "permission"
                      ? t("Permissions")
                      : t("Matcher")}
                </span>
                <Combobox
                  aria-label={t("Hook matcher")}
                  value={form.matcher}
                  onInput={(value) => setForm("matcher", value)}
                  options={matcherOptions()}
                  complete={lastAlternative}
                  disabled={readonly || busy()}
                />
                <Show
                  when={neverRuns(target(), form.matcher)}
                  fallback={
                    <small class="muted">
                      {compares() === "tool"
                        ? t("Tool names such as bash or read|edit, or a pattern.")
                        : compares() === "permission"
                          ? t("Permission names such as bash or edit, or a pattern.")
                          : compares() === "value"
                            ? t("Pick from the list, or type a pattern.")
                            : compares() === "nothing"
                              ? t("This event has nothing to match, so the matcher is ignored.")
                              : t("Use * for every tool, or tool names such as Bash|Edit. Events without a tool ignore this field.")}
                    </small>
                  }
                >
                  <small class="hook-never" role="status">
                    <Icon name="alert" size="xs" />
                    {neverRunsText(target())}
                  </small>
                </Show>
              </label>
            </Show>
            <label class="field">
              <span>{t("Command")}</span>
              <textarea
                aria-label={t("Hook command")}
                rows={4}
                value={form.command}
                onInput={(e) => setForm("command", e.currentTarget.value)}
              />
            </label>
            <label class="field">
              <span>{t("Timeout (seconds)")}</span>
              <input
                aria-label={t("Hook timeout")}
                type="number"
                min="1"
                max="600"
                step="1"
                value={form.timeout}
                onInput={(e) => setForm("timeout", e.currentTarget.value)}
              />
            </label>
            <details open={!!form.args || !!form.shell || form.async}>
              <summary>{t("Advanced")}</summary>
              <label class="field">
                <span>{t("Arguments, one per line")}</span>
                <textarea
                  aria-label={t("Hook arguments")}
                  rows={3}
                  value={form.args}
                  onInput={(e) => setForm("args", e.currentTarget.value)}
                />
                <small class="muted">
                  {t(
                    "With arguments, Command is an executable. Without arguments, it runs through a shell.",
                  )}
                </small>
              </label>
              <label class="field">
                <span>{t("Shell (optional)")}</span>
                <input
                  aria-label={t("Hook shell")}
                  value={form.shell}
                  placeholder="sh"
                  onInput={(e) => setForm("shell", e.currentTarget.value)}
                />
              </label>
              <label class="management-toggle">
                <input
                  type="checkbox"
                  checked={form.async}
                  onChange={(e) => setForm("async", e.currentTarget.checked)}
                />
                {t("Run asynchronously")}
              </label>
            </details>
          </fieldset>
        </>
      )}
    </FormDialog>
  );
}
