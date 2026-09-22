import type { ProviderInfo } from "@haarness/sdk";
import { modelName } from "../model-names";
import { ProviderIcon } from "./ProviderIcon";
import type { SelectOption } from "./Select";
import { reasoningLevels } from "./reasoning";

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
