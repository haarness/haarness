import type { JsonValue } from "./types.js";
export type CapabilityKind = "skill" | "agent" | "mcp" | "plugin" | "extension";
export interface CapabilityRequest {
  action: "list" | "get" | "create" | "edit" | "import" | "enable" | "disable" | "remove" | "install" | "marketplace" | "update";
  kind?: CapabilityKind;
  name?: string;
  scope?: "local" | "global";
  [key: string]: JsonValue | undefined;
}
export interface MarketplaceRequest {
  action: "search" | "collections" | "detail" | "versions" | "manifest" | "installed" | "install" | "update" | "enable" | "disable" | "remove";
  id?: string;
  kind?: CapabilityKind;
  q?: string;
  collection?: string;
  publisher?: string;
  cursor?: string;
  limit?: number;
  version?: number;
  name?: string;
  scope?: "local" | "global";
  values?: Record<string, string>;
  /** Authorized Cloud install manifest, useful for private account resources on remote runtimes. */
  manifest?: JsonValue;
  options?: JsonValue;
}
