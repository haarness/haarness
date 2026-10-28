import { t } from "./i18n";
import { createMemo, createSignal, For, Show } from "solid-js";
import { Select } from "./Select";
import { Icon } from "./icons";
import type { ModelOption } from "./provider-model-options";
import {
  carriedReasoning,
  reasoningChoice,
  reasoningLabel,
  reasoningLevels,
} from "./reasoning";
import "./model-picker.css";

/** Model selection and model-specific reasoning share one accessible popover. */
export function ModelPicker(props: {
  value: string;
  options: ModelOption[];
  effort?: string;
  /** The level the server's `defaults.reasoning_effort` sets, which a turn
   * with no effort chosen runs at; the model's default is then a choice of
   * its own (`MODEL_DEFAULT_EFFORT`). */
  configured?: string | null;
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
  const effort = () => props.effort ?? "";
  const choice = createMemo(() =>
    reasoningChoice(effort(), levels(), props.configured),
  );
  const steps = () => choice().steps;
  const valid = () => choice().current;
  const index = () => choice().index;
  const unavailable = () => choice().unavailable;
  async function choose(value: string) {
    if (busy() || props.disabled) return;
    const next = props.options.find(
      (option) => String(option.value) === value,
    );
    if (!next || next.disabled) return;
    setBusy(true);
    try {
      await props.onChange(value);
      // A failed asynchronous model change must not mutate the old model's effort.
      if (props.value === value && props.onEffortChange)
        props.onEffortChange(
          carriedReasoning(effort(), next.reasoningEfforts, props.configured),
        );
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
                  <Show when={choice().configured}>
                    <span class="reasoning-default">{t("default")}</span>
                  </Show>
                </strong>
                <small>{selected()?.label ?? props.value}</small>
              </div>
              <button
                type="button"
                class="icon-button"
                aria-label={t("Reset reasoning to model default")}
                title={t("Use model default")}
                disabled={(!valid() && !unavailable()) || busy() || props.disabled}
                onClick={() => props.onEffortChange?.(choice().modelDefault)}
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
                      steps()[Number(event.currentTarget.value)] ?? choice().modelDefault,
                    )
                  }
                />
                <span class="reasoning-thumb" aria-hidden="true" />
              </div>
              <div class="reasoning-scale" aria-hidden="true">
                <span>{t(props.configured ? "Model default" : "Default")}</span>
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
