import type { ProviderInfo, Thread } from "@haarness/sdk";

export interface ModelKey {
  provider: string;
  model: string;
}

export function parseModel(value?: string | null): ModelKey | undefined {
  if (!value) return;
  const slash = value.indexOf("/");
  if (slash <= 0 || slash === value.length - 1) return;
  return { provider: value.slice(0, slash), model: value.slice(slash + 1) };
}

export function readModelRecents(value: unknown): ModelKey[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter(
      (item): item is ModelKey =>
        !!item &&
        typeof item.provider === "string" &&
        !!item.provider &&
        typeof item.model === "string" &&
        !!item.model,
    )
    .slice(0, 20)
    .map((item) => ({ provider: item.provider, model: item.model }));
}

/** Resolve before creating a thread, so the displayed model is the one sent.
 * Every choice must belong to a connected provider's current catalog. */
export function selectModel(input: {
  providers: ProviderInfo[];
  explicit?: Partial<ModelKey>;
  agentModel?: string | null;
  defaultProvider?: string | null;
  defaultModel?: string | null;
  recent: ModelKey[];
  threads: Thread[];
}): ModelKey | undefined {
  const connected = input.providers.filter(
    (p) => p.authenticated && p.models.length,
  );
  const valid = (key?: ModelKey) =>
    key &&
    connected.some(
      (p) => p.id === key.provider && p.models.some((m) => m.id === key.model),
    );
  const explicit =
    input.explicit?.provider && input.explicit.model
      ? { provider: input.explicit.provider, model: input.explicit.model }
      : undefined;
  if (valid(explicit)) return explicit;
  const agent = parseModel(input.agentModel);
  if (valid(agent)) return agent;
  const qualified = parseModel(input.defaultModel);
  const configured = input.defaultModel
    ? input.defaultProvider
      ? { provider: input.defaultProvider, model: input.defaultModel }
      : valid(qualified)
        ? qualified
        : connected.flatMap((p) =>
            p.models
              .filter((m) => m.id === input.defaultModel)
              .map((m) => ({ provider: p.id, model: m.id })),
          )[0]
    : undefined;
  if (valid(configured)) return configured;
  const history = [
    ...input.recent,
    ...input.threads
      .filter((t) => !t.isSubagent)
      .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt)),
  ];
  // A provider-only preference should keep its own recent/preferred model.
  const provider =
    !input.defaultModel &&
    connected.find((p) => p.id === input.defaultProvider);
  if (provider) {
    const recent = history.find(
      (key) => key.provider === provider.id && valid(key),
    );
    const model = provider.models.find((m) => m.tools) ?? provider.models[0];
    return recent
      ? { provider: recent.provider, model: recent.model }
      : { provider: provider.id, model: model.id };
  }
  const recent = history.find((key) => valid(key));
  if (recent) return { provider: recent.provider, model: recent.model };
  // Preserve the server's preferred order within each catalog. Keep the
  // built-in provider preference deterministic, not dependent on discovery timing.
  const providers = [...connected].sort(
    (a, b) =>
      Number(b.id === "openai") - Number(a.id === "openai") ||
      a.id.localeCompare(b.id),
  );
  for (const tools of [true, false]) {
    for (const provider of providers) {
      const model = provider.models.find((m) => !tools || m.tools);
      if (model) return { provider: provider.id, model: model.id };
    }
  }
}
