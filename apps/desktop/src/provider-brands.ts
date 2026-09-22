/** Presentation identity, deliberately independent of API protocol or model maker.
 * Compatible gateways retain their own logo; unknown/custom IDs use a neutral mark.
 */
export const providerBrands = {
  openai: "openai",
  anthropic: "anthropic",
  gemini: "gemini",
  google: "gemini",
  azure: "azure",
  "google-vertex": "vertexai",
  "github-copilot": "githubcopilot",
  "amazon-bedrock": "bedrock",
  groq: "groq",
  mistral: "mistral",
  xai: "xai",
  deepseek: "deepseek",
  togetherai: "together",
  together: "together",
  "fireworks-ai": "fireworks",
  fireworks: "fireworks",
  openrouter: "openrouter",
  cerebras: "cerebras",
  sambanova: "sambanova",
  nvidia: "nvidia",
  zai: "zai",
  deepinfra: "deepinfra",
  huggingface: "huggingface",
  perplexity: "perplexity",
  moonshotai: "moonshot",
  siliconflow: "siliconcloud",
  "siliconflow-cn": "siliconcloud",
  nebius: "nebius",
  "novita-ai": "novita",
  novita: "novita",
  "ollama-cloud": "ollama",
  ollama: "ollama",
  lmstudio: "lmstudio",
  llama: "meta",
  modelscope: "modelscope",
  upstage: "upstage",
  digitalocean: "digitalocean",
  wandb: "wandb",
} as const;

export function providerBrand(id?: string | null): string | undefined {
  const key = id?.trim().toLowerCase();
  return key && Object.hasOwn(providerBrands, key)
    ? providerBrands[key as keyof typeof providerBrands]
    : undefined;
}
