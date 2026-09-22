import type { ProviderInfo } from "@haarness/sdk";
import names from "../../../crates/haarness-protocol/src/model-names.json";
export function modelDisplayName(id: string, supplied?: string | null): string {
  if (supplied?.trim() && supplied !== id) return supplied;
  return (names as Record<string, string>)[id] ?? id;
}
export function modelName(
  provider: string | undefined,
  id: string | undefined,
  providers: ProviderInfo[],
): string {
  if (!id) return "";
  return modelDisplayName(
    id,
    providers.find((p) => p.id === provider)?.models.find((m) => m.id === id)
      ?.displayName,
  );
}
