import type { ProviderInfo } from "@haarness/sdk";
import names from "../../../crates/haarness-protocol/src/model-names.json";
export function modelDisplayName(id: string, supplied?: string | null): string {
  if (supplied?.trim() && supplied !== id) return supplied;
  const table = names as Record<string, string>;
  // Routers name a model `provider/model`; the bare id may be known.
  const base = claudeBaseId(id);
  const bare = id.slice(id.lastIndexOf("/") + 1);
  return (
    table[id] ??
    table[bare] ??
    (base ? table[base] : undefined) ??
    tidyName(base ?? bare) ??
    id
  );
}
/** Brands a model id starts with, as their names spell them; `true` joins
 * the version with a hyphen (`GPT-7`). Mirrors `BRANDS` in
 * crates/haarness-protocol/src/model_names.rs. */
const BRANDS: Record<string, [string, boolean]> = {
  gpt: ["GPT", true],
  glm: ["GLM", true],
  claude: ["Claude", false],
  gemini: ["Gemini", false],
  gemma: ["Gemma", false],
  grok: ["Grok", false],
  kimi: ["Kimi", false],
  mistral: ["Mistral", false],
  devstral: ["Devstral", false],
  codestral: ["Codestral", false],
  sonar: ["Sonar", false],
  deepseek: ["DeepSeek", false],
  qwen: ["Qwen", false],
  llama: ["Llama", false],
};
/** A readable name for a model id the table does not know yet, from its
 * brand: `gpt-7-mini` is "GPT-7 Mini", `claude-opus-6-1` "Claude Opus 6.1".
 * Mirrors `tidy_name` in crates/haarness-protocol/src/model_names.rs. */
export function tidyName(id: string): string | undefined {
  const [first, ...tokens] = id.split(/[-@]/);
  let name: string;
  let joined = false;
  if (Object.hasOwn(BRANDS, first)) [name, joined] = BRANDS[first];
  else if (/^o\d+$/.test(first)) name = first;
  else return;
  const words: string[] = [];
  let date: string | undefined;
  let inVersion = false;
  for (const token of tokens) {
    if (!/^[a-z0-9.]+$/.test(token)) return;
    const digits = /^\d+$/.test(token);
    if (digits && token.length === 8) {
      date = `(${token.slice(0, 4)}-${token.slice(4, 6)}-${token.slice(6)})`;
      continue;
    }
    if (token === "latest") continue;
    if (digits && token.length <= 2) {
      if (inVersion && words.length) words[words.length - 1] += "." + token;
      else words.push(token);
      inVersion = true;
      continue;
    }
    inVersion = false;
    words.push(tidyWord(token));
  }
  if (joined && words.length && (/^\d/.test(words[0]) || words[0] === "OSS"))
    name = `${name}-${words.shift()}`;
  return [name, ...words, ...(date ? [date] : [])].join(" ");
}
function tidyWord(token: string) {
  if (token === "oss") return "OSS";
  if (/^[\d.]+[bkm]$/.test(token)) return token.toUpperCase();
  return /^[a-z]/.test(token) ? token[0].toUpperCase() + token.slice(1) : token;
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
