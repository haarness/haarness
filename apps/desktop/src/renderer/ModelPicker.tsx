import { t } from "./i18n";
import { createSignal, For, Show } from "solid-js";
import { Select } from "./Select";
import { Icon } from "./icons";
import type { ModelOption } from "./provider-model-options";
import { reasoningLabel, reasoningLevels, validReasoning } from "./reasoning";
import "./model-picker.css";

/** Model selection and model-specific reasoning share one accessible popover. */
export function ModelPicker(props: {
  value: string;
  options: ModelOption[];
  effort?: string;
  onChange: (value: string) => unknown;
  onEffortChange?: (value: string) => void;
  onOpen?: () => void;
  disabled?: boolean;
  class?: string;
  "aria-label"?: string;
}) {
  const [busy, setBusy] = createSignal(false);
  const selected = () =>
    props.options.find((option) => String(option.value) === props.value);
  const levels = () => reasoningLevels(selected()?.reasoningEfforts);
  const steps = () => ["", ...levels()];
  const effort = () => props.effort ?? "";
  const valid = () => validReasoning(effort(), levels());
  const index = () => Math.max(0, steps().indexOf(valid()));
  const unavailable = () => !!effort() && valid() !== effort();
  async function choose(value: string) {
    // The model shown, picked again, is not a change: nothing is written and
    // the rows stay as they are.
    if (busy() || props.disabled || value === props.value) return;
    const next = props.options.find(
      (option) => String(option.value) === value,
    );
    if (!next || next.disabled) return;
    setBusy(true);
    try {
      await props.onChange(value);
      // A failed asynchronous model change must not mutate the old model's effort.
      if (props.value === value && props.onEffortChange)
        props.onEffortChange(validReasoning(effort(), next.reasoningEfforts));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Select
      aria-label={props["aria-label"] ?? "Model"}
      class={props.class}
      value={props.value}
      disabled={props.disabled}
      options={
        busy()
          ? props.options.map((option) => ({ ...option, disabled: true }))
          : props.options
      }
      searchable
      stayOpenOnSelect
      onOpen={props.onOpen}
      onChange={(value) => void choose(value)}
      triggerSuffix={
        <Show when={props.onEffortChange && levels().length && valid()}>
          <span class="model-effort-summary">{t(reasoningLabel(valid()))}</span>
        </Show>
      }
      footer={(close) => (
        <div class="model-picker-detail" aria-busy={busy()}>
          <Show when={props.onEffortChange}>
            <div class="reasoning-heading">
              <Icon name="reasoning" size="md" />
              <div class="reasoning-current">
                <strong>
                  {unavailable()
                    ? t("{name} (unavailable)", { name: t(reasoningLabel(effort())) })
                    : t(reasoningLabel(valid()))}
                </strong>
                <small>{selected()?.label ?? props.value}</small>
              </div>
              <button
                type="button"
                class="icon-button"
                aria-label={t("Reset reasoning to model default")}
                title={t("Use model default")}
                disabled={!effort() || busy() || props.disabled}
                onClick={() => props.onEffortChange?.("")}
              >
                <Icon name="refresh" size="md" />
              </button>
            </div>
            <Show
              when={levels().length}
              fallback={
                <p class="reasoning-unavailable">
                  {props.value
                    ? t("This model has no adjustable reasoning levels.")
                    : t("Choose a model to adjust reasoning.")}
                </p>
              }
            >
              <div
                class="reasoning-slider"
                style={{
                  "--reasoning-fraction": String(index() / levels().length),
                }}
              >
                <span class="reasoning-fill" aria-hidden="true" />
                <div class="reasoning-ticks" aria-hidden="true">
                  <For each={steps()}>
                    {(_, tick) => (
                      <span classList={{ passed: tick() <= index() }} />
                    )}
                  </For>
                </div>
                <input
                  type="range"
                  min="0"
                  max={levels().length}
                  step="1"
                  value={index()}
                  disabled={busy() || props.disabled}
                  aria-label={t("Reasoning effort")}
                  aria-valuetext={t(reasoningLabel(valid()))}
                  onInput={(event) =>
                    props.onEffortChange?.(
                      steps()[Number(event.currentTarget.value)] ?? "",
                    )
                  }
                />
                <span class="reasoning-thumb" aria-hidden="true" />
              </div>
              <div class="reasoning-scale" aria-hidden="true">
                <span>{t("Default")}</span>
                <span>{t(reasoningLabel(levels().at(-1) ?? ""))}</span>
              </div>
            </Show>
          </Show>
          <button type="button" class="model-picker-done" onClick={close}>
            {t("Done")}
          </button>
        </div>
      )}
    />
  );
}
