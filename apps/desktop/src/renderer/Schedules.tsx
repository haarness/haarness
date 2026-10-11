import { t, language } from "./i18n";
import { ProviderIcon } from "./ProviderIcon";
import { ModelPicker } from "./ModelPicker";
import { conversationAgents } from "./agent-selection";
import { providerModelOptions } from "./provider-model-options";
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  onMount,
  Show,
} from "solid-js";
import { createStore } from "solid-js/store";
import { Select } from "./Select";
import { useApp } from "./state";
import { Button, confirm, dismissOnBackdrop, Empty, Icon } from "./ui";
import { parseModel, selectModel } from "./model-selection";
import {
  describeSchedule,
  triggerArgs,
  triggerForm,
  units,
  weekdays,
  type Frequency,
  type Schedule,
} from "./schedule-data";

export function Schedules(props: {
  cli: (args: string[]) => Promise<unknown>;
}) {
  const app = useApp();
  const [rows, setRows] = createSignal<Schedule[]>([]);
  const [editing, setEditing] = createSignal<{ item?: Schedule }>();
  const [busy, setBusy] = createSignal(false);
  const workspace = () => app.state.connection?.workspace;
  async function refresh() {
    const values = (await props.cli([
      "schedule",
      "list",
      "--json",
    ])) as Schedule[];
    setRows(values.filter((item) => item.workspace === workspace()));
  }
  createEffect(() => {
    void app.run(refresh);
  });
  async function change(args: string[]) {
    setBusy(true);
    try {
      await props.cli(args);
      await refresh();
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <p class="muted">
        {t("Run a prompt on a schedule in {project}. Chats appear under Scheduled in the sidebar.", { project: app.project()?.label ?? t("this project") })}
      </p>
      <div class="toolbar">
        <button
          class="primary"
          disabled={busy()}
          data-setting="Create schedule" onClick={() => setEditing({})}
        >
          {t("Create schedule")}
        </button>
      </div>
      <Show when={!rows().length && !editing()}>
        <Empty
          title={t("No schedules yet")}
          detail={t("Run a prompt every morning, every hour, or on any cron schedule.")}
        />
      </Show>
      <For each={rows()}>
        {(item) => (
          <div class="setting-block schedule-card" data-setting={"schedules:" + item.id}>
            <header>
              <h3>
                <Icon name="clock" size="sm" />
                {item.name}
              </h3>
              <span class="badge">
                {t(item.enabled ? "Enabled" : "Disabled")}
              </span>
            </header>
            <p>{item.prompt}</p>
            <p class="muted">{describeSchedule(item.trigger, t, language())}</p>
            <div class="toolbar">
              <button disabled={busy()} onClick={() => setEditing({ item })}>
                {t("Edit")}
              </button>
              <button
                disabled={busy()}
                onClick={() =>
                  app.run(() =>
                    change([
                      "schedule",
                      item.enabled ? "disable" : "enable",
                      item.id,
                    ]),
                  )
                }
              >
                {t(item.enabled ? "Disable" : "Enable")}
              </button>
              <button
                disabled={busy()}
                onClick={() =>
                  app.run(() => change(["schedule", "run", item.id]))
                }
              >
                {t("Run now")}
              </button>
              <button
                disabled={busy()}
                onClick={() =>
                  app.run(async () => {
                    if (await confirm(t("Remove schedule?"), item.name, { submit: t("Remove schedule"), danger: true }))
                      await change(["schedule", "remove", item.id]);
                  })
                }
              >
                {t("Remove")}
              </button>
            </div>
          </div>
        )}
      </For>
      <Show when={editing()} keyed>
        {(value) => (
          <ScheduleForm
            item={value.item}
            save={async (args) => {
              await change(args);
            }}
            close={() => setEditing()}
          />
        )}
      </Show>
    </>
  );
}

function ScheduleForm(props: {
  item?: Schedule;
  save: (args: string[]) => Promise<void>;
  close: () => void;
}) {
  const app = useApp();
  const item = props.item;
  const agents = () => conversationAgents(app.state.agents);
  const initialAgent =
    item?.agent ??
    agents().find((agent) => agent.id === "build")?.id ??
    agents()[0]?.id ??
    "build";
  const resolve = (agent: string) =>
    selectModel({
      providers: app.state.providers,
      agentModel: agents().find((value) => value.id === agent)?.model,
      defaultProvider: app.state.connection?.info.defaultProvider,
      defaultModel: app.state.connection?.info.defaultModel,
      recent: app.state.modelRecents,
      threads: app.state.threads,
    });
  const initialModel =
    item?.model ??
    (() => {
      const model = resolve(initialAgent);
      return model ? `${model.provider}/${model.model}` : "";
    })();
  const [form, setForm] = createStore({
    name: item?.name ?? "",
    prompt: item?.prompt ?? "",
    agent: initialAgent,
    model: initialModel,
    variant: item?.variant ?? "",
    enabled: item?.enabled ?? false,
    timezone: item?.timezone ?? "",
    ...triggerForm(item?.trigger),
  });
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal("");
  const models = createMemo(() => providerModelOptions(app.state.providers));
  const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const timezones = () =>
    [
      ...new Set([
        "",
        zone,
        "UTC",
        form.timezone,
        ...Intl.supportedValuesOf("timeZone"),
      ]),
    ].map((value) => ({
      value,
      label: value || t("Computer timezone ({zone})", { zone }),
    }));
  let dialog!: HTMLDialogElement;
  onMount(() => dialog.showModal());
  const close = () => {
    if (!dialog.isConnected || !dialog.open) return;
    dialog.close();
    props.close();
  };
  const submit = async (event: SubmitEvent) => {
    event.preventDefault();
    if (saving()) return;
    setError("");
    try {
      if (!form.name.trim() || !form.prompt.trim())
        throw new Error("Add a name and prompt.");
      if (!models().some((model) => model.value === form.model))
        throw new Error(
          models().length
            ? "Choose a connected model."
            : "Connect a provider before creating a schedule.",
        );
      // Keep an unchanged one-shot instant, including seconds and its UTC offset.
      const trigger =
        item?.trigger.type === "once" &&
        form.frequency === "once" &&
        form.at === triggerForm(item.trigger).at
          ? ["--at", item.trigger.at]
          : triggerArgs(form);
      const args = [
        "schedule",
        item ? "update" : "add",
        ...(item ? [item.id] : []),
        "--name",
        form.name.trim(),
        "--prompt",
        form.prompt.trim(),
        ...trigger,
        "--model",
        form.model,
        "--agent",
        form.agent,
      ];
      // An explicit empty value clears a previous effort on update.
      if (item || form.variant) args.push("--variant", form.variant);
      if (app.state.platform === "linux" && form.frequency !== "once")
        args.push("--timezone", form.timezone);
      if (item) args.push(form.enabled ? "--enable" : "--disable");
      else if (!form.enabled) args.push("--disabled");
      setSaving(true);
      await props.save(args);
      close();
    } catch (error) {
      setError(t(error instanceof Error ? error.message : String(error)));
    } finally {
      setSaving(false);
    }
  };
  return (
    <dialog
      ref={dialog}
      class="dialog schedule-dialog"
      aria-label={t(item ? "Edit schedule" : "Create schedule")}
      {...dismissOnBackdrop(close)}
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
    >
      <form onSubmit={submit}>
        <header>
          <div>
            <h2>{t(item ? "Edit schedule" : "Create schedule")}</h2>
            <p class="muted small">
              {app.project()?.label ?? app.state.connection?.workspace}
            </p>
          </div>
          <Button
            icon="close"
            label={t("Close dialog")}
            onClick={close}
          />
        </header>
        <fieldset disabled={saving()}>
          <label class="field">
            <span>{t("Name")}</span>
            <input
              autofocus
              required
              value={form.name}
              onInput={(event) => setForm("name", event.currentTarget.value)}
              placeholder={t("Daily project check")}
            />
          </label>
          <label class="field">
            <span>{t("Prompt")}</span>
            <textarea
              rows={3}
              required
              value={form.prompt}
              onInput={(event) =>
                setForm("prompt", event.currentTarget.value)
              }
              placeholder={t("What should Haarness do?")}
            />
          </label>
          <div class="schedule-grid">
            <label class="field">
              <span>{t("Frequency")}</span>
              <Select
                aria-label={t("Frequency")}
                value={form.frequency}
                onChange={(value) => setForm("frequency", value as Frequency)}
                options={[
                  { value: "daily", label: t("Every day") },
                  { value: "weekdays", label: t("Weekdays") },
                  { value: "weekly", label: t("Weekly") },
                  { value: "interval", label: t("At an interval") },
                  { value: "once", label: t("Once") },
                  { value: "cron", label: t("Custom cron") },
                ]}
              />
            </label>
            <Show
              when={["daily", "weekdays", "weekly"].includes(form.frequency)}
            >
              <label class="field">
                <span>{t("Time")}</span>
                <input
                  type="time"
                  required
                  value={form.time}
                  onInput={(event) =>
                    setForm("time", event.currentTarget.value)
                  }
                />
              </label>
            </Show>
            <Show when={form.frequency === "weekly"}>
              <label class="field">
                <span>{t("Day")}</span>
                <Select
                  aria-label={t("Day")}
                  value={form.day}
                  onChange={(value) => setForm("day", Number(value))}
                  options={weekdays.map((label, value) => ({ label: t(label), value }))}
                />
              </label>
            </Show>
            <Show when={form.frequency === "interval"}>
              <div class="schedule-interval">
                <label class="field">
                  <span>{t("Every")}</span>
                  <input
                    type="number"
                    min="1"
                    step="1"
                    required
                    value={form.amount}
                    onInput={(event) =>
                      setForm("amount", Number(event.currentTarget.value))
                    }
                  />
                </label>
                <label class="field">
                  <span>{t("Unit")}</span>
                  <Select
                    aria-label={t("Interval unit")}
                    value={form.unit}
                    onChange={(value) => setForm("unit", Number(value))}
                    options={units.map((unit) => ({ ...unit, label: t(unit.label) }))}
                  />
                </label>
              </div>
            </Show>
            <Show when={form.frequency === "once"}>
              <label class="field">
                <span>{t("Date and time")}</span>
                <input
                  type="datetime-local"
                  required
                  value={form.at}
                  onInput={(event) =>
                    setForm("at", event.currentTarget.value)
                  }
                />
              </label>
            </Show>
            <Show when={form.frequency === "cron"}>
              <label class="field">
                <span>{t("Cron expression")}</span>
                <input
                  required
                  value={form.cron}
                  onInput={(event) =>
                    setForm("cron", event.currentTarget.value)
                  }
                  placeholder="0 9 * * *"
                />
              </label>
            </Show>
          </div>
          <Show
            when={app.state.platform === "linux" && form.frequency !== "once"}
            fallback={
              <p class="schedule-timezone small muted">
                {t("Times follow your computer’s timezone · {zone}", { zone })}
              </p>
            }
          >
            <label class="field">
              <span>{t("Timezone")}</span>
              <Select
                aria-label={t("Timezone")}
                value={form.timezone}
                onChange={(value) => setForm("timezone", value)}
                options={timezones()}
                searchable
              />
            </label>
          </Show>
          <div class="schedule-grid">
            <label class="field">
              <span>{t("Agent")}</span>
              <Select
                aria-label={t("Agent")}
                value={form.agent}
                options={[
                  ...agents().map((agent) => ({
                    value: agent.id,
                    label: agent.name || agent.id,
                  })),
                  ...(agents().some((agent) => agent.id === form.agent)
                    ? []
                    : [
                        {
                          value: form.agent,
                          label: t("{name} (unavailable)", { name: form.agent }),
                        },
                      ]),
                ]}
                onChange={(value) => {
                  setForm("agent", value);
                  const model = resolve(value);
                  if (model)
                    setForm({
                      model: `${model.provider}/${model.model}`,
                      variant: "",
                    });
                }}
              />
            </label>
            <label class="field">
              <span>{t("Model")}</span>
              <Show
                when={models().length || form.model}
                fallback={
                  <button
                    type="button"
                    onClick={() => {
                      props.close();
                      app.openConnect();
                    }}
                  >
                    {t("Connect a provider")}
                  </button>
                }
              >
                <ModelPicker
                  aria-label={t("Model")}
                  value={form.model}
                  options={[
                    ...models(),
                    ...(form.model &&
                    !models().some((option) => option.value === form.model)
                      ? [
                          {
                            value: form.model,
                            label: t("{name} (unavailable)", { name: form.model }),
                            disabled: true,
                            triggerIcon: () => (
                              <ProviderIcon
                                provider={parseModel(form.model)?.provider}
                              />
                            ),
                          },
                        ]
                      : []),
                  ]}
                  effort={form.variant}
                  onEffortChange={(value) => setForm("variant", value)}
                  onOpen={() => void app.run(() => app.catalogs(true))}
                  onChange={(value) => setForm("model", value)}
                />
              </Show>
            </label>
          </div>
          <label class="schedule-enabled">
            <input
              type="checkbox"
              role="switch"
              checked={form.enabled}
              onChange={(event) =>
                setForm("enabled", event.currentTarget.checked)
              }
            />
            <span>
              {t("Enable schedule")}
              <small class="muted">{t("Runs even while Haarness is closed.")}</small>
            </span>
          </label>
        </fieldset>
        <Show when={error()}>
          <p class="notice-inline" role="alert">
            {error()}
          </p>
        </Show>
        <footer>
          <button type="button" onClick={close}>
            {t("Cancel")}
          </button>
          <button class="primary" type="submit" disabled={saving()}>
            {t(saving() ? "Saving…" : "Save")}
          </button>
        </footer>
      </form>
    </dialog>
  );
}
