/*
 * The line Settings → MCP servers shows under a server's name: why it
 * failed, or, for one that connected, the tools it left out. The reasons
 * Haarness writes read in the person's language; a server's own words, and
 * the names of its tools, stay as they are.
 */
import { t } from "./i18n";

export type McpServerNote = { text: string; quiet: boolean };

export function mcpServerNote(server: { status: string; detail?: string | null }): McpServerNote | undefined {
  const detail = server.detail?.trim();
  if (!detail) return undefined;
  if (server.status === "failed") return { text: failureReason(detail), quiet: false };
  // A working server says it only when it left tools out.
  const left = /; left out as not valid MCP tools: (.+)$/.exec(detail);
  return left ? { text: t("Left out as not valid MCP tools: {tools}", { tools: left[1] }), quiet: true } : undefined;
}

function failureReason(detail: string) {
  if (detail === "Its URL is not a web address (http or https) with a host.")
    return t("Its URL is not a web address (http or https) with a host.");
  const one = /^Its URL names an environment variable that is not set: (.+)\.$/.exec(detail);
  if (one) return t("Its URL names an environment variable that is not set: {variable}.", { variable: one[1] });
  const many = /^Its URL names environment variables that are not set: (.+)\.$/.exec(detail);
  if (many) return t("Its URL names environment variables that are not set: {variables}.", { variables: many[1] });
  return detail;
}
