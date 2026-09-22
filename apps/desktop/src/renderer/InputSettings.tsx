import { t } from "./i18n";
import { Show, createEffect, createSignal, onCleanup, untrack } from "solid-js";
import { Select } from "./Select";
import { useApp } from "./state";

interface InputSettingsValue {
  delivery: "steer" | "queue";
  scopeDelivery: "steer" | "queue" | null;
  inheritedDelivery: "steer" | "queue";
  source: string;
  revision: string;
  message?: string;
}

/** The setting belongs to the connected server/project, not Electron storage. */
export function InputSettings() {
  const app = useApp();
  const [scope, setScope] = createSignal("project");
  const projectConfig = () => app.state.connection?.info.projectTrusted !== false;
  createEffect(() => { if (!projectConfig()) setScope("global"); });
  const [value, setValue] = createSignal<InputSettingsValue>();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const supported = () => app.state.connection?.info.capabilities.includes("input-settings.v1");
  let generation = 0;
  async function request(input: Record<string, unknown> = { action: "get" }, reset = false) {
    const connection = app.state.connection?.id;
    if (!connection || !supported() || (busy() && !reset)) return;
    const current = ++generation;
    setBusy(true);
    if (reset) setValue(undefined);
    try {
      const result = await window.haarness.api<{ data: InputSettingsValue }>(connection, "manageResource", ["input-settings", { ...input, scope: scope() }]);
      if (current !== generation || connection !== app.state.connection?.id) return;
      setValue(result.data);
      setError("");
      if (result.data.message) app.notify(result.data.message);
    } catch (cause) {
      if (current === generation) setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (current === generation) setBusy(false);
    }
  }
  createEffect(() => {
    void app.state.connection?.id;
    void scope();
    untrack(() => void request({ action: "get" }, true));
  });
  // This pane also reflects edits from an attached TUI or another server daemon.
  const timer = setInterval(() => void request(), 2000);
  onCleanup(() => { generation++; clearInterval(timer); });
  return (
    <div class="setting-row input-settings-row" data-setting="Messages sent while working">
      <div>
        <strong>{t("Messages sent while working")}</strong>
        <p>{t("Steer joins the next agent step. Queue waits until current work finishes.")}</p>
        <Show when={value()}><p>{t("Current project")}: {t(value()!.delivery === "steer" ? "Steer" : "Queue")} · {value()!.source === "default" ? t("Default") : value()!.source}</p></Show>
        <Show when={!supported()}><p>{t("Open a project on a server that supports input settings.")}</p></Show>
        <Show when={error()}><p role="alert">{error()}</p></Show>
      </div>
      <div class="input-settings-controls">
        <Select aria-label={t("Input mode scope")} value={scope()} onChange={setScope} disabled={busy() || !supported()}
          options={[...(projectConfig() ? [{ value: "project", label: t("This project") }] : []), { value: "global", label: t("Global") }]} />
        <Select aria-label={t("Messages sent while working")} value={value()?.scopeDelivery ?? "inherit"}
          disabled={busy() || !value() || !supported()}
          onChange={(delivery) => void request({ action: "set", delivery, revision: value()?.revision })}
          options={[
            { value: "inherit", label: `${t("Use inherited setting")} (${t(value()?.inheritedDelivery === "queue" ? "Queue" : "Steer")})` },
            { value: "steer", label: t("Steer") },
            { value: "queue", label: t("Queue") },
          ]} />
      </div>
    </div>
  );
}
