import type { ManagementView, ProviderInfo } from "@haarness/sdk";

/** One way of connecting a provider, as the server describes it. */
export interface ConnectMethod {
  id: string;
  kind: "oauth" | "api";
  label: string;
  description: string;
}

/** A provider row in the connect picker. */
export interface ConnectProvider {
  id: string;
  name: string;
  connected: boolean;
  models: number;
  /** "oauth" | "api" | "environment" | "local" when connected. */
  source: string | null;
  methods: ConnectMethod[];
  keyUrl: string | null;
  env: string[];
  note: string | null;
  popular: number | null;
}

const text = (value: unknown) => (typeof value === "string" ? value : "");

/** The picker catalog from the server's `connect` view, tolerant of gaps. */
export function parseCatalog(view: ManagementView | undefined): ConnectProvider[] {
  const data = view?.data as { providers?: unknown } | undefined;
  if (!Array.isArray(data?.providers)) return [];
  return data.providers.flatMap((row): ConnectProvider[] => {
    if (!row || typeof row !== "object") return [];
    const item = row as Record<string, unknown>;
    const id = text(item.id);
    if (!id) return [];
    const methods = Array.isArray(item.methods)
      ? item.methods.flatMap((method): ConnectMethod[] => {
          const entry = method as Record<string, unknown>;
          const kind = entry?.kind === "oauth" ? "oauth" : entry?.kind === "api" ? "api" : null;
          const methodId = text(entry?.id);
          return kind && methodId
            ? [{ id: methodId, kind, label: text(entry.label) || methodId, description: text(entry.description) }]
            : [];
        })
      : [];
    return [
      {
        id,
        name: text(item.name) || id,
        connected: item.connected === true,
        models: typeof item.models === "number" ? item.models : 0,
        source: text(item.source) || null,
        methods,
        keyUrl: /^https?:\/\//.test(text(item.keyUrl)) ? text(item.keyUrl) : null,
        env: Array.isArray(item.env) ? item.env.filter((v): v is string => typeof v === "string") : [],
        note: text(item.note) || null,
        popular: typeof item.popular === "number" ? item.popular : null,
      },
    ];
  });
}

/** What the picker shows when the server predates the connect catalog. */
export function fallbackCatalog(providers: ProviderInfo[]): ConnectProvider[] {
  return providers.map((provider) => ({
    id: provider.id,
    name: provider.name,
    connected: provider.authenticated,
    models: provider.models.length,
    source: null,
    methods: [{ id: "api", kind: "api", label: "Use an API key", description: "" }],
    keyUrl: null,
    env: [],
    note: null,
    popular: null,
  }));
}

/** Rows matching the search, in catalog order. */
export function filterProviders(list: ConnectProvider[], query: string) {
  const needle = query.trim().toLowerCase();
  if (!needle) return list;
  return list.filter((provider) => `${provider.id} ${provider.name}`.toLowerCase().includes(needle));
}

/** The popular shelf keeps the server's order; the rest reads alphabetically. */
export function groupProviders(list: ConnectProvider[]) {
  const popular = list
    .filter((provider) => provider.popular !== null)
    .sort((a, b) => (a.popular ?? 0) - (b.popular ?? 0));
  const other = list
    .filter((provider) => provider.popular === null)
    .sort((a, b) => a.name.localeCompare(b.name));
  return { popular, other };
}

/**
 * A browser callback only reaches the server when they share a machine, so a
 * remote server offers the device code in its place.
 */
export function usableMethods(provider: ConnectProvider, local: boolean) {
  return provider.methods.filter((method) => local || method.id !== "browser");
}

/** "API key", "GitHub token", … from the provider's own key method. */
export function keyLabel(provider: ConnectProvider) {
  const method = provider.methods.find((candidate) => candidate.kind === "api");
  return method ? method.label.replace(/^Use an? /, "") : "API key";
}

/** The pieces of a connected row's status line, for the caller to translate. */
export function connectionSummary(provider: ConnectProvider): {
  how: "signed in" | "API key" | "from the environment" | "";
  models: number;
} {
  const how =
    provider.source === "oauth"
      ? "signed in"
      : provider.source === "api" || provider.source === "legacy api"
        ? "API key"
        : provider.source === "environment"
          ? "from the environment"
          : "";
  return { how, models: provider.models };
}

/** The state a sign-in poll can report. */
export type LoginStatus = "pending" | "connected" | "failed" | "cancelled";

export interface LoginData {
  id: string;
  url: string;
  code: string;
  method: string;
  state: LoginStatus;
  error: string | null;
}

/** The structured half of a login view. */
export function parseLogin(view: ManagementView | undefined): LoginData | null {
  const data = view?.data as Record<string, unknown> | undefined;
  if (!data || typeof data !== "object") return null;
  const id = text(data.id);
  if (!id) return null;
  const state = text(data.state);
  return {
    id,
    url: text(data.url),
    code: text(data.code),
    method: text(data.method) || "browser",
    state:
      state === "connected" || state === "failed" || state === "cancelled"
        ? state
        : "pending",
    error: text(data.error) || null,
  };
}
