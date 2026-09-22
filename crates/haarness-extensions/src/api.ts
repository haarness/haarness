/** Haarness extension API v1. Type-only: no SDK install or runtime import required. */
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type Schema = { [key: string]: Json };
export interface Context {
  readonly workspace: string;
  readonly extension: string;
  readonly options: Json;
}
export interface ToolContext extends Context {
  readonly threadId: string;
  readonly turnId: string;
}
export interface ToolResult { text: string; metadata?: Json }
export interface Tool<Args = Record<string, Json>> {
  description: string;
  /** JSON Schema; validated by Rust before execute is called. */
  parameters: Schema;
  execute(args: Args, context: ToolContext): string | ToolResult | Promise<string | ToolResult>;
}
type Reply<T> = T | void | Promise<T | void>;
export interface Hooks {
  prompt?(input: { threadId: string; prompt: string }, context: Context): Reply<{ context?: string; block?: string }>;
  /** Append system instructions or change reasoning effort; tool schemas and history stay runtime-owned. */
  chat?(input: { threadId: string; model: string; system: string; reasoningEffort: string | null }, context: Context): Reply<{ context?: string; reasoningEffort?: string }>;
  /** Sampling limits for this provider request, after `chat`. `null` clears a value; transports apply what they support. */
  "chat.params"?(
    input: { threadId: string; model: string; temperature: number | null; topP: number | null; topK: number | null; maxOutputTokens: number | null; reasoningEffort: string | null; options: Record<string, Json> },
    context: Context,
  ): Reply<{ temperature?: number | null; topP?: number | null; topK?: number | null; maxOutputTokens?: number | null; reasoningEffort?: string; options?: Record<string, Json> }>;
  /** Extra HTTP headers for HTTP provider transports (OpenAI-compatible, Anthropic, Gemini, OpenAI); later extensions override earlier ones. */
  "chat.headers"?(input: { threadId: string; model: string; headers: Record<string, string> }, context: Context): Reply<{ headers?: Record<string, string> }>;
  "tool.before"?(input: { threadId: string; tool: string; arguments: Json }, context: Context): Reply<{ arguments?: Json; context?: string; block?: string }>;
  "tool.after"?(input: { threadId: string; tool: string; arguments: Json; output: string; isError: boolean }, context: Context): Reply<{ context?: string }>;
  /** Rewrite a tool's model-facing description or JSON Schema, once per tool when the registry is composed. Arguments are still validated against the tool's own contract. */
  "tool.definition"?(input: { toolID: string; description: string; parameters: Schema }, context: Context): Reply<{ description?: string; parameters?: Schema }>;
  /** Answer a permission prompt before the user sees it. Omit `status` or return "ask" to keep the prompt; a thrown error denies. */
  "permission.ask"?(input: { threadId: string; permission: string; detail: Json }, context: Context): Reply<{ status?: "allow" | "deny" | "ask"; reason?: string }>;
  /** Observe or abort a slash command before its template expands. */
  "command.execute.before"?(input: { threadId: string; command: string; arguments: string }, context: Context): Reply<{ block?: string; context?: string }>;
  /** Extra environment variables for one bash tool invocation. */
  "shell.env"?(input: { threadId: string; cwd: string }, context: Context): Reply<{ env?: Record<string, string> }>;
  "session.start"?(input: { threadId: string; agent: string; directory: string }, context: Context): Reply<never>;
  /** A thread was archived. */
  "session.end"?(input: { threadId: string; reason: string; directory: string }, context: Context): Reply<never>;
  "chat.turn.end"?(input: { threadId: string; turnId: string; agent: string; provider: string; model: string; finishReason: string }, context: Context): Reply<never>;
  /** Effective runtime configuration summary, once per loaded generation. */
  config?(input: { workspace: string; [key: string]: Json }, context: Context): Reply<never>;
  stop?(input: { threadId: string }, context: Context): Reply<never>;
  /** Receives every lifecycle hook above as `{type, ...input}`, before the specific hook. */
  event?(input: { type: string; [key: string]: unknown }, context: Context): Reply<never>;
}
export interface Extension {
  apiVersion: 1;
  /** Lowercase letters, digits and underscores, at most 24 characters. */
  name: string;
  tools?: Record<string, Tool<any>>;
  hooks?: Hooks;
}
export type ExtensionFactory = (context: Context) => Extension | Promise<Extension>;
