// Private stdio transport. Extension console output never shares the RPC channel.
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";
import { format } from "node:util";

const write = process.stdout.write.bind(process.stdout);
for (const key of ["log", "info", "warn", "error", "debug"] as const) {
  console[key] = (...args: unknown[]) => process.stderr.write(format(...args) + "\n");
}
const reply = (value: unknown) => write(JSON.stringify(value) + "\n");
// Result contract per hook: accepted keys and the value kind Rust reads for each.
type Kind = "string" | "number" | "object" | "record" | "status" | "json";
const CONTRACTS: Record<string, Record<string, Kind>> = {
  prompt: { context: "string", block: "string" },
  chat: { context: "string", reasoningEffort: "string" },
  "chat.params": { temperature: "number", topP: "number", topK: "number", maxOutputTokens: "number", reasoningEffort: "string", options: "object" },
  "chat.headers": { headers: "record" },
  "tool.before": { context: "string", block: "string", arguments: "json" },
  "tool.after": { context: "string" },
  "tool.definition": { description: "string", parameters: "object" },
  "permission.ask": { status: "status", reason: "string" },
  "command.execute.before": { block: "string", context: "string" },
  "shell.env": { env: "record" },
  "session.start": {},
  "session.end": {},
  "chat.turn.end": {},
  config: {},
  stop: {},
  event: {},
};
const isRecord = (value: unknown) => typeof value === "object" && value !== null && !Array.isArray(value);
function check(name: string, key: string, value: unknown) {
  const kind = CONTRACTS[name]?.[key];
  if (!kind) throw new Error(`Unsupported ${name} hook result: ${key}`);
  if (value === null || value === undefined || kind === "json") return;
  const ok =
    kind === "string" ? typeof value === "string"
    : kind === "number" ? typeof value === "number" && Number.isFinite(value)
    : kind === "object" ? isRecord(value)
    : kind === "record" ? isRecord(value) && Object.values(value as object).every((item) => typeof item === "string")
    : value === "allow" || value === "deny" || value === "ask";
  if (!ok) {
    const expected = kind === "status" ? '"allow", "deny" or "ask"' : kind === "record" ? "a string map" : kind === "object" ? "an object" : `a ${kind}`;
    throw new Error(`${name} hook result ${key} must be ${expected}`);
  }
}
let extension: any;
let context: any;
const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of lines) {
  let request: any;
  try {
    request = JSON.parse(line);
    let result: unknown;
    if (request.method === "load") {
      context = Object.freeze(request.context);
      const module = await import(pathToFileURL(request.source).href);
      extension = typeof module.default === "function"
        ? await module.default(context)
        : module.default;
      if (extension?.apiVersion !== 1) throw new Error("Extension must export apiVersion: 1");
      const hooks = extension.hooks ?? {};
      for (const [name, hook] of Object.entries(hooks)) {
        if (!Object.hasOwn(CONTRACTS, name) || typeof hook !== "function") {
          throw new Error(`Unsupported hook: ${name}`);
        }
      }
      result = {
        apiVersion: 1,
        name: extension.name,
        hooks: Object.keys(hooks),
        tools: Object.entries(extension.tools ?? {}).map(([name, value]: [string, any]) => {
          if (typeof value.execute !== "function") throw new Error(`Tool ${name} needs execute`);
          return { name, description: value.description, input_schema: value.parameters };
        }),
      };
    } else if (request.method === "tool") {
      const tool = extension.tools[request.name];
      result = await tool.execute(request.arguments, Object.freeze({ ...context, ...request.context }));
      if (typeof result === "string") result = { text: result, metadata: {} };
      if (typeof (result as any)?.text !== "string") throw new Error("Tools must return a string or {text, metadata}");
      result = { metadata: {}, ...(result as object) };
    } else if (request.method === "hook") {
      if (!Object.hasOwn(CONTRACTS, request.name)) throw new Error(`Unknown hook: ${request.name}`);
      if (extension.hooks?.event && request.name !== "event") {
        await extension.hooks.event({ type: request.name, ...request.input }, context);
      }
      result = await extension.hooks?.[request.name]?.(request.input, context) ?? {};
      if (typeof result !== "object" || Array.isArray(result)) throw new Error("Hooks must return an object or nothing");
      for (const [key, value] of Object.entries(result as object)) check(request.name, key, value);
    } else {
      throw new Error("Unknown host method");
    }
    reply({ id: request.id, result });
  } catch (error) {
    reply({ id: request?.id, error: error instanceof Error ? error.message : String(error) });
  }
}
