import { t } from "./i18n";
import { ModelPicker } from "./ModelPicker";
import { conversationAgents } from "./agent-selection";
import { providerModelOptions } from "./provider-model-options";
import { ProviderIcon } from "./ProviderIcon";
import { Show } from "solid-js";
import { useApp } from "./state";
import { Select } from "./Select";
import { modelName } from "../model-names";
import { parseModel } from "./model-selection";

export function ExecutionFields(props: {
  model: string;
  agent?: string;
  variant?: string;
  disabled: boolean;
  setModel: (value: string) => void;
  setAgent?: (value: string) => void;
  setVariant?: (value: string) => void;
}) {
  const app = useApp();
  const agents = () => conversationAgents(app.state.agents);
  const models = () => providerModelOptions(app.state.providers);
  const currentName = () => {
    const key = parseModel(props.model);
    return key
      ? modelName(key.provider, key.model, app.state.providers)
      : props.model;
  };
  return (
    <div class="form-grid">
      <Show when={props.setAgent}>
        <label class="field">
          <span>Agent</span>
          <Select
            aria-label="Agent"
            value={props.agent ?? ""}
            disabled={props.disabled}
            options={[
              { value: "", label: "Use session agent" },
              ...agents().map((agent) => ({
                value: agent.id,
                label: agent.name || agent.id,
              })),
              ...(props.agent &&
              !agents().some((agent) => agent.id === props.agent)
                ? [
                    {
                      value: props.agent,
                      label: `${props.agent} (unavailable)`,
                      disabled: true,
                    },
                  ]
                : []),
            ]}
            onChange={props.setAgent}
          />
        </label>
      </Show>
      <label class="field">
        <span>Model</span>
        <ModelPicker
          aria-label="Model"
          value={props.model}
          effort={props.variant}
          onEffortChange={props.setVariant}
          disabled={props.disabled}
          options={[
            { value: "", label: "Use session model" },
            ...models(),
            ...(props.model &&
            !models().some((model) => model.value === props.model)
              ? [
                  {
                    value: props.model,
                    label: `${currentName()} (unavailable)`,
                    triggerIcon: () => (
                      <ProviderIcon
                        provider={parseModel(props.model)?.provider}
                      />
                    ),
                    disabled: true,
                  },
                ]
              : []),
          ]}
          onChange={props.setModel}
        />
        <Show when={!models().length}>
          <small class="muted">
            {t("Connect a provider to choose a specific model.")}
          </small>
        </Show>
      </label>
    </div>
  );
}
