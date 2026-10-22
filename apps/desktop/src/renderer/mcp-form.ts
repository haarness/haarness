export interface McpPair {
  key: string;
  value: string;
  /** Keep the value in the system's keychain and write only a reference. */
  vault?: boolean;
}

export interface McpDraft {
  name: string;
  type: "local" | "remote";
  scope: "global" | "local";
  enabled: boolean;
  command: string;
  args: McpPair[];
  environment: McpPair[];
  url: string;
  headers: McpPair[];
  oauth: "auto" | "enabled" | "disabled";
  oauthClientId: string;
  oauthScopes: string;
}

export function newMcpDraft(): McpDraft {
  return {
    name: "",
    type: "local",
    scope: "global",
    enabled: true,
    command: "",
    args: [],
    environment: [],
    url: "",
    headers: [],
    oauth: "auto",
    oauthClientId: "",
    oauthScopes: "",
  };
}

function pairs(rows: McpPair[], kind: "environment" | "headers") {
  const seen = new Set<string>();
  const entries: [string, string][] = [];
  for (const [index, row] of rows.entries()) {
    const key = row.key.trim();
    if (!key && !row.value) continue;
    const label = kind === "environment" ? "Environment variable" : "Header";
    if (!key) throw Error(`${label} ${index + 1} needs a name.`);
    const valid =
      kind === "environment"
        ? /^[A-Za-z_][A-Za-z0-9_]*$/.test(key)
        : /^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(key);
    if (!valid) throw Error(`${label} ${index + 1} has an invalid name.`);
    const identity = kind === "headers" ? key.toLowerCase() : key;
    if (seen.has(identity))
      throw Error(
        `${label} ${index + 1} repeats a name. Keep one row per name.`,
      );
    if (kind === "headers" && /[\x00-\x08\x0a-\x1f\x7f]/.test(row.value))
      throw Error(
        `Header ${index + 1} contains an invalid character in its value.`,
      );
    if (row.value.includes("\0"))
      throw Error(`${label} ${index + 1} contains a null character.`);
    seen.add(identity);
    entries.push([key, row.value]);
  }
  return Object.fromEntries(entries);
}

/** Preserve argument boundaries and values; never interpret shell syntax. */
export function mcpInput(draft: McpDraft): Record<string, unknown> {
  const name = draft.name.trim();
  if (!name || /[\x00-\x1f\x7f]/.test(name))
    throw Error("Enter a server name without control characters.");
  const common = {
    action: "add",
    name,
    type: draft.type,
    scope: draft.scope,
    enabled: draft.enabled,
  };
  if (draft.type === "local") {
    const command = draft.command.trim();
    if (!command || command.includes("\0"))
      throw Error("Enter an executable, such as npx, uvx, or a full path.");
    const args: string[] = [];
    for (const [index, row] of draft.args.entries()) {
      if (!row.key && !row.value) continue;
      if (!row.key)
        throw Error(
          `Argument ${index + 1} needs a flag or positional argument.`,
        );
      if (row.key.includes("\0") || row.value.includes("\0"))
        throw Error(`Argument ${index + 1} contains a null character.`);
      args.push(row.key);
      if (row.value !== "") args.push(row.value);
    }
    return {
      ...common,
      command,
      args,
      environment: pairs(draft.environment, "environment"),
    };
  }
  const url = draft.url.trim();
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    throw Error("Enter a valid HTTP or HTTPS server URL.");
  }
  if (!["http:", "https:"].includes(parsed.protocol))
    throw Error("The server URL must use HTTP or HTTPS.");
  if (parsed.username || parsed.password)
    throw Error(
      "Use a request header for credentials instead of putting them in the URL.",
    );
  return {
    ...common,
    url,
    headers: pairs(draft.headers, "headers"),
    ...(draft.oauth !== "auto" ? { oauth: draft.oauth === "enabled" } : {}),
    ...(draft.oauth !== "disabled"
      ? {
          ...(draft.oauthClientId.trim()
            ? { oauthClientId: draft.oauthClientId.trim() }
            : {}),
          oauthScopes: [
            ...new Set(draft.oauthScopes.split(/\s+/).filter(Boolean)),
          ],
        }
      : {}),
  };
}

/** Names that usually hold a credential: a token, key, password, cookie… */
const SECRET_NAME = /token|secret|passw|key|auth|bearer|credential|cookie|session|private|signature/i;

/**
 * Whether a row's value looks like something to keep out of a settings
 * file: a credential-named variable or header with a value, or a long
 * unbroken run of letters and digits. A reference (`${…}`) already is.
 */
export function secretLooking(row: McpPair) {
  const value = row.value.trim();
  if (!value || value.includes("${")) return false;
  if (SECRET_NAME.test(row.key)) return true;
  return /^(?=.*\d)(?=.*[A-Za-z])[A-Za-z0-9_\-.~+/=]{24,}$/.test(value);
}

/** The keychain entry a server's row is kept as: `<server>-<name>`, in the
 * characters an entry name may use, starting with a letter or digit. */
export function keychainEntry(server: string, key: string) {
  const name = `${server.trim()}-${key.trim()}`
    .replace(/[^A-Za-z0-9._-]+/g, "-")
    .replace(/^[^A-Za-z0-9]+/, "")
    .slice(0, 120);
  return name || "mcp-secret";
}

/**
 * The secrets a draft keeps in the keychain and the input that names them
 * instead: each row marked to keep and still holding a plain value becomes
 * `${keychain:<entry>}`, and a header's `Bearer`, `Basic` or `Token` scheme
 * stays written before it, so the file still reads as what it is.
 */
export function keychainInput(draft: McpDraft) {
  const input = mcpInput(draft);
  const secrets: { name: string; secret: string }[] = [];
  const kind = draft.type === "local" ? "environment" : "headers";
  const rows = draft.type === "local" ? draft.environment : draft.headers;
  const values = input[kind] as Record<string, string>;
  for (const row of rows) {
    const key = row.key.trim();
    if (!row.vault || !secretLooking(row) || !(key in values)) continue;
    const name = keychainEntry(String(input.name), key);
    const scheme = kind === "headers" ? /^(Bearer|Basic|Token)\s+(\S.*)$/i.exec(row.value) : null;
    const secret = scheme ? scheme[2] : row.value;
    secrets.push({ name, secret });
    values[key] = scheme ? `${scheme[1]} \${keychain:${name}}` : `\${keychain:${name}}`;
  }
  return { input, secrets };
}

/** Points the reference to keychain entry `from` in `input`'s environment or
 * headers at entry `to`, which the keychain gave when `from` was taken. */
export function renameKeychainReference(input: Record<string, unknown>, from: string, to: string) {
  for (const kind of ["environment", "headers"]) {
    const values = input[kind] as Record<string, string> | undefined;
    if (!values) continue;
    for (const [key, value] of Object.entries(values))
      values[key] = value.split(`\${keychain:${from}}`).join(`\${keychain:${to}}`);
  }
}
