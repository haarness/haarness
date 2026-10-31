import type { ProviderInfo } from "@haarness/sdk";
import { modelName } from "../model-names";
import { ProviderIcon } from "./ProviderIcon";
import type { SelectOption } from "./Select";
import { t } from "./i18n";
import { unlistedModel } from "./model-selection";
import { REASONING_SCALE, reasoningLevels } from "./reasoning";

export interface ModelOption extends SelectOption {
  reasoningEfforts?: string[];
}
export function providerModelOptions(
  providers: ProviderInfo[],
): ModelOption[] {
  return providers
    .filter((provider) => provider.authenticated)
    .flatMap((provider) =>
      provider.models.map((model) => ({
        value: `${provider.id}/${model.id}`,
        label: modelName(provider.id, model.id, providers),
        group: provider.name || provider.id,
        triggerIcon: () => <ProviderIcon provider={provider.id} />,
        groupIcon: () => <ProviderIcon provider={provider.id} size="sm" />,
        reasoningEfforts: reasoningLevels(model.reasoningEfforts),
      })),
    );
}
/** `options` with the chat's model `value` kept in view, first in its
 * provider's group with a quiet note, when that provider no longer lists it.
 * Every reasoning level stays on offer for it; the server fits the one
 * chosen. */
export function withUnlistedModel(
  options: ModelOption[],
  providers: ProviderInfo[],
  value: string,
): ModelOption[] {
  const key = unlistedModel(providers, value);
  const provider = key && providers.find((p) => p.id === key.provider);
  if (!key || !provider) return options;
  const kept: ModelOption = {
    value,
    label: modelName(key.provider, key.model, providers),
    hint: t("no longer listed"),
    group: provider.name || provider.id,
    triggerIcon: () => <ProviderIcon provider={provider.id} />,
    groupIcon: () => <ProviderIcon provider={provider.id} size="sm" />,
    reasoningEfforts: [...REASONING_SCALE],
  };
  const at = options.findIndex((option) => option.group === kept.group);
  return at < 0
    ? [kept, ...options]
    : [...options.slice(0, at), kept, ...options.slice(at)];
}
