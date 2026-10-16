import type { ProviderInfo } from "@haarness/sdk";
import names from "../../../crates/haarness-protocol/src/model-names.json";
export function modelDisplayName(id: string, supplied?: string | null): string {
  if (supplied?.trim() && supplied !== id) return supplied;
  const table = names as Record<string, string>;
  // Routers name a model `provider/model`; the bare id may be known.
  const base = claudeBaseId(id);
  return (
    table[id] ??
    table[id.slice(id.lastIndexOf("/") + 1)] ??
    (base ? table[base] : undefined) ??
    id
  );
}
// Bedrock writes a Claude id with a region and vendor prefix and a version tag
// (`apac.anthropic.claude-sonnet-5-5-v1:0`, `anthropic.claude-opus-4-6-v1`) in
// more combinations than the table lists; this is the bare id it names. Mirrors
// `claude_base_id` in crates/haarness-protocol/src/model_names.rs.
const CLAUDE_ID = /^(?:(?:[a-z-]+\.)?anthropic\.)?(claude-.+?)(?:-v\d+)?(?::\d+)?$/;
function claudeBaseId(id: string): string | undefined {
  const match = CLAUDE_ID.exec(id.slice(id.lastIndexOf("/") + 1));
  return match && match[1] !== id ? match[1] : undefined;
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
