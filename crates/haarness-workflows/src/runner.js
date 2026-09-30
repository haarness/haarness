/*
 * Haarness native workflow child runtime.
 *
 * This file is embedded in the Rust executable and evaluated by Bun. Stdio is
 * reserved for newline-delimited JSON protocol frames: stdout for child frames,
 * stdin for host frames, and stderr for diagnostics. Never log to stdout.
 */

const VERSION = 1
const pending = new Map()
let nextRequestID = 1

function send(frame) {
  process.stdout.write(`${JSON.stringify({ version: VERSION, ...frame })}\n`)
}

function fail(message) {
  const error = message instanceof Error ? message.stack || message.message : String(message)
  send({ type: "error", error })
}

function abortError(reason) {
  const error = new Error(reason === undefined ? "Workflow host request aborted" : String(reason))
  error.name = "AbortError"
  return error
}

function encodeValue(value) {
  if (value === undefined) return { kind: "undefined" }
  const seen = new WeakSet()
  let json
  try {
    json = JSON.stringify(value, (_key, nested) => {
      if (typeof nested === "number" && !Number.isFinite(nested)) {
        throw new TypeError("Workflow values cannot contain non-finite numbers")
      }
      if (typeof nested === "bigint") {
        throw new TypeError("Workflow values cannot contain bigint values")
      }
      if (nested && typeof nested === "object") {
        if (seen.has(nested)) throw new TypeError("Workflow values cannot contain cycles")
        seen.add(nested)
      }
      return nested
    })
  } catch (error) {
    throw new TypeError(`Workflow value is not JSON-serializable: ${error}`)
  }
  if (json === undefined) throw new TypeError("Workflow value is not JSON-serializable")
  return { kind: "json", json }
}

function decodeValue(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("Invalid workflow value envelope")
  }
  if (value.kind === "undefined") return undefined
  if (value.kind !== "json" || typeof value.json !== "string") {
    throw new TypeError("Invalid workflow value envelope")
  }
  return JSON.parse(value.json)
}

async function* inputLines() {
  const decoder = new TextDecoder()
  let buffered = ""
  for await (const chunk of Bun.stdin.stream()) {
    buffered += decoder.decode(chunk, { stream: true })
    for (;;) {
      const newline = buffered.indexOf("\n")
      if (newline < 0) break
      const line = buffered.slice(0, newline).replace(/\r$/, "")
      buffered = buffered.slice(newline + 1)
      if (line.length > 0) yield line
    }
  }
  buffered += decoder.decode()
  if (buffered.length > 0) yield buffered.replace(/\r$/, "")
}

function parseHostFrame(line) {
  const frame = JSON.parse(line)
  if (!frame || typeof frame !== "object" || Array.isArray(frame) || frame.version !== VERSION) {
    throw new TypeError("Invalid workflow host frame")
  }
  return frame
}

function request(method, params, signal) {
  if (signal?.aborted) return Promise.reject(abortError(signal.reason))
  const id = nextRequestID++
  return new Promise((resolve, reject) => {
    const onAbort = () => {
      if (!pending.delete(id)) return
      send({ type: "abort", id, reason: String(signal.reason ?? "aborted") })
      reject(abortError(signal.reason))
    }
    if (signal) signal.addEventListener("abort", onAbort, { once: true })
    pending.set(id, {
      resolve(value) {
        if (signal) signal.removeEventListener("abort", onAbort)
        resolve(value)
      },
      reject(error) {
        if (signal) signal.removeEventListener("abort", onAbort)
        reject(error)
      },
    })
    send({ type: "request", id, method, params })
  })
}

function notice(method, value) {
  send({ type: "notice", method, value })
}

function agentReply(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return value
  if (typeof value.text !== "string" || typeof value.continuation !== "string" || !("data" in value)) {
    return value
  }
  const result = { text: value.text, data: value.data }
  Object.defineProperties(result, {
    followup: {
      enumerable: false,
      value(input) {
        const raw = input && typeof input === "object" && !Array.isArray(input) ? input : input
        const signal = raw?.signal instanceof AbortSignal ? raw.signal : undefined
        const params = raw && typeof raw === "object" && !Array.isArray(raw) ? { ...raw } : raw
        if (params && typeof params === "object" && !Array.isArray(params)) delete params.signal
        return request("agent.followup", { token: value.continuation, input: params }, signal)
          .then((next) => (next === null ? null : agentReply(next)))
      },
    },
    close: {
      enumerable: false,
      value: () => request("agent.close", { token: value.continuation }).then(() => undefined),
    },
  })
  return result
}

function context(runID) {
  const runTask = async (task) => (typeof task === "function" ? await task() : await task)
  const parallel = async (tasks, options = {}) => {
    if (!Array.isArray(tasks)) throw new TypeError("parallel() expects an array")
    if (!options || typeof options !== "object" || Array.isArray(options)) {
      throw new TypeError("parallel() options must be an object")
    }
    if (tasks.length === 0) return []
    const requested = options.concurrencyLimit ?? tasks.length
    if (!Number.isFinite(requested) || requested < 1) {
      throw new TypeError("parallel() concurrencyLimit must be a positive number")
    }
    const limit = Math.max(1, Math.floor(requested))
    const result = new Array(tasks.length).fill(null)
    let index = 0
    async function worker() {
      while (index < tasks.length) {
        const current = index++
        try {
          result[current] = await runTask(tasks[current])
        } catch (error) {
          notice("log", `Parallel task ${current + 1} failed: ${error}`)
        }
      }
    }
    await Promise.all(Array.from({ length: Math.min(limit, tasks.length) }, () => worker()))
    return result
  }

  return {
    runID,
    budgetRemaining: Infinity,
    budget: {
      total: null,
      spent: () => 0,
      remaining: () => Infinity,
      tokensTotal: null,
      tokensSpent: () => 0,
      tokensRemaining: () => Infinity,
    },
    setPhase(phase) {
      if (typeof phase !== "string" || phase.length === 0) {
        throw new TypeError("setPhase() expects a non-empty string")
      }
      notice("phase", phase)
    },
    setStatus(value) {
      if (typeof value === "string") value = { message: value }
      if (!value || typeof value !== "object" || Array.isArray(value)) {
        throw new TypeError("setStatus() expects a string or object")
      }
      notice("status", value)
    },
    log(value) {
      notice("log", String(value))
    },
    parallel,
    async pipeline(items, ...rest) {
      if (!Array.isArray(items)) throw new TypeError("pipeline() expects an array as the first argument")
      const tail = rest.at(-1)
      const options = tail && typeof tail === "object" && typeof tail !== "function" ? rest.pop() : undefined
      if (rest.length === 0 || rest.some((stage) => typeof stage !== "function")) {
        throw new TypeError("pipeline() stages must be functions")
      }
      return parallel(
        items.map((item, index) => async () => {
          let value = item
          for (const stage of rest) value = await stage(value, item, index)
          return value
        }),
        options,
      )
    },
    agent(input) {
      const raw = input && typeof input === "object" && !Array.isArray(input) ? input : undefined
      const signal = raw?.signal instanceof AbortSignal ? raw.signal : undefined
      const params = raw ? { ...raw } : input
      if (params && typeof params === "object" && !Array.isArray(params)) delete params.signal
      return request("agent", params, signal).then(agentReply)
    },
    tool(name, args = {}, options = {}) {
      if (typeof name !== "string" || name.length === 0) throw new TypeError("tool() expects a tool name")
      return request("tool", { name, args, options })
    },
    shell(command, opts = {}) {
      if (typeof command !== "string" || command.length === 0) {
        throw new TypeError("shell() expects a non-empty command")
      }
      return request("shell", { command, opts })
    },
    workflow(name, args = {}) {
      if (typeof name !== "string" || name.length === 0) {
        throw new TypeError("workflow() expects a workflow name")
      }
      return request("workflow", { name, args })
    },
    async question() {
      throw new Error("Workflow human questions are not supported in this runtime yet")
    },
  }
}

const pluginSpecifiers = ["@haarness-ai/plugin", "@opencode-ai/plugin"]

function installPluginRuntime() {
  const exports = {
    workflow(input) {
      if (!input || typeof input !== "object" || Array.isArray(input)) {
        throw new TypeError("workflow() expects an object")
      }
      return {
        meta: {
          name: input.name,
          description: input.description,
          whenToUse: input.whenToUse,
          phases: input.phases,
          arguments: input.arguments,
        },
        run: input.run,
      }
    },
    tool(input) {
      return input
    },
  }
  Bun.plugin({
    name: "haarness-workflow-plugin-runtime",
    setup(builder) {
      for (const specifier of pluginSpecifiers) {
        builder.module(specifier, () => ({ exports, loader: "object" }))
      }
    },
  })
  globalThis.__haarnessWorkflow = exports.workflow
}

function normalizedSource(source) {
  const trimmed = source.trim()
  if (/^(?:await\s+)?workflow\s*\(/.test(trimmed) || trimmed.startsWith("{")) {
    return `const workflow = globalThis.__haarnessWorkflow; export default (${trimmed});`
  }
  return source
}

async function load(source, sourcePath, runID) {
  installPluginRuntime()
  let url
  let blob = false
  if (sourcePath !== undefined) {
    if (typeof sourcePath !== "string" || sourcePath.length === 0) {
      throw new TypeError("Invalid workflow sourcePath")
    }
    const { pathToFileURL } = await import("node:url")
    url = `${pathToFileURL(sourcePath).href}?haarnessWorkflow=${encodeURIComponent(runID)}-${Date.now()}`
  } else {
    const transpiler = new Bun.Transpiler({ loader: "ts", target: "bun" })
    const javascript = transpiler.transformSync(normalizedSource(source))
    url = URL.createObjectURL(new Blob([javascript], { type: "text/javascript" }))
    blob = true
  }
  try {
    const module = await import(url)
    const candidate = module.default?.meta && module.default?.run
      ? module.default
      : module.meta && module.run
        ? module
        : undefined
    if (!candidate) {
      throw new Error("Workflow module must export default workflow({...}) or { meta, run }")
    }
    if (!candidate.meta || typeof candidate.meta !== "object" || Array.isArray(candidate.meta)) {
      throw new Error("Workflow metadata must be an object")
    }
    if (typeof candidate.meta.name !== "string" || candidate.meta.name.length === 0) {
      throw new Error("Workflow metadata requires a non-empty name")
    }
    if (typeof candidate.run !== "function") throw new Error("Workflow run export must be a function")
    return candidate.run
  } finally {
    if (blob) URL.revokeObjectURL(url)
  }
}

const portableBlocked = new Set([
  "Bun", "Deno", "Function", "Intl", "Atomics", "Buffer", "SharedArrayBuffer",
  "WebSocket", "XMLHttpRequest", "clearInterval", "clearTimeout", "console",
  "crypto", "eval", "fetch", "globalThis", "module", "navigator",
  "performance", "process", "queueMicrotask", "require", "setInterval",
  "setTimeout", "Date",
])

function skipTrivia(source, from) {
  let index = from
  for (;;) {
    while (/\s/.test(source[index] ?? "")) index++
    if (source.startsWith("//", index)) {
      const newline = source.indexOf("\n", index + 2)
      index = newline < 0 ? source.length : newline + 1
      continue
    }
    if (source.startsWith("/*", index)) {
      const end = source.indexOf("*/", index + 2)
      if (end < 0) throw new SyntaxError("Unterminated comment before portable metadata")
      index = end + 2
      continue
    }
    return index
  }
}

function portableBody(source) {
  let index = skipTrivia(source, 0)
  const prefix = /^export\s+const\s+meta\s*=/.exec(source.slice(index))
  if (!prefix) {
    throw new SyntaxError("Portable workflow must begin with export const meta = { ... }")
  }
  index += prefix[0].length
  index = skipTrivia(source, index)
  if (source[index] !== "{") {
    throw new SyntaxError("Portable workflow metadata must be a literal object")
  }
  let depth = 0
  let state = "normal"
  for (; index < source.length; index++) {
    const char = source[index]
    const next = source[index + 1]
    if (state === "line") {
      if (char === "\n") state = "normal"
      continue
    }
    if (state === "block") {
      if (char === "*" && next === "/") {
        state = "normal"
        index++
      }
      continue
    }
    if (state === "single" || state === "double" || state === "template") {
      const terminator = state === "single" ? "'" : state === "double" ? '"' : "`"
      if (char === "\\") index++
      else if (char === terminator) state = "normal"
      continue
    }
    if (char === "/" && next === "/") {
      state = "line"
      index++
      continue
    }
    if (char === "/" && next === "*") {
      state = "block"
      index++
      continue
    }
    if (char === "'") {
      state = "single"
      continue
    }
    if (char === '"') {
      state = "double"
      continue
    }
    if (char === "`") {
      state = "template"
      continue
    }
    if (char === "{" || char === "[" || char === "(") depth++
    if (char === "}" || char === "]" || char === ")") {
      depth--
      if (depth === 0) {
        index++
        break
      }
      if (depth < 0) throw new SyntaxError("Invalid portable workflow metadata")
    }
  }
  if (depth !== 0 || state !== "normal") {
    throw new SyntaxError("Unterminated portable workflow metadata")
  }
  index = skipTrivia(source, index)
  if (source[index] === ";") index++
  return source.slice(index).trimStart()
}

function portableIdentifiers(source) {
  const identifiers = []
  let state = "normal"
  let templateExpressionDepth = 0
  for (let index = 0; index < source.length; index++) {
    const char = source[index]
    const next = source[index + 1]
    if (state === "line") {
      if (char === "\n") state = "normal"
      continue
    }
    if (state === "block") {
      if (char === "*" && next === "/") {
        state = "normal"
        index++
      }
      continue
    }
    if (state === "single" || state === "double") {
      const terminator = state === "single" ? "'" : '"'
      if (char === "\\") index++
      else if (char === terminator) state = "normal"
      continue
    }
    if (state === "template") {
      if (char === "\\") index++
      else if (char === "`") state = "normal"
      else if (char === "$" && next === "{") {
        state = "normal"
        templateExpressionDepth = 1
        index++
      }
      continue
    }
    if (char === "/" && next === "/") {
      state = "line"
      index++
      continue
    }
    if (char === "/" && next === "*") {
      state = "block"
      index++
      continue
    }
    if (char === "'") {
      state = "single"
      continue
    }
    if (char === '"') {
      state = "double"
      continue
    }
    if (char === "`") {
      state = "template"
      continue
    }
    if (templateExpressionDepth > 0) {
      if (char === "{") templateExpressionDepth++
      if (char === "}" && --templateExpressionDepth === 0) {
        state = "template"
        continue
      }
    }
    if (/[A-Za-z_$]/.test(char)) {
      let end = index + 1
      while (/[A-Za-z0-9_$]/.test(source[end] ?? "")) end++
      identifiers.push({ name: source.slice(index, end), start: index, end })
      index = end - 1
    }
  }
  return identifiers
}

function validatePortable(body) {
  const identifiers = portableIdentifiers(body)
  for (const identifier of identifiers) {
    let before = identifier.start - 1
    while (/\s/.test(body[before] ?? "")) before--
    let after = identifier.end
    while (/\s/.test(body[after] ?? "")) after++
    const propertyName = body[before] === "." || body[after] === ":"
    const methodName = body[after] === "(" && (body[before] === "{" || body[before] === ",")
    if (!propertyName && identifier.name === "import") {
      throw new Error("Portable workflows cannot import modules")
    }
    if (!propertyName && identifier.name === "export") {
      throw new Error("Portable workflows cannot export from the script body")
    }
    if (!propertyName && !methodName && portableBlocked.has(identifier.name)) {
      throw new Error(`Portable workflow global ${identifier.name} is unavailable`)
    }
  }
  for (let index = 0; index + 1 < identifiers.length; index++) {
    const left = identifiers[index]
    const right = identifiers[index + 1]
    if (left.name === "Math" && right.name === "random") {
      const separator = body.slice(left.end, right.start).replace(/\s/g, "")
      if (separator === ".") {
        throw new Error("Portable workflows cannot access Math.random")
      }
    }
  }
}

function portableWrapper(body, bridgeName) {
  return `
    (async function(${bridgeName}) {
      "use strict"
      const copy = (value) => {
        if (value === undefined) return undefined
        return JSON.parse(JSON.stringify(value))
      }
      const decode = (value) => {
        const response = JSON.parse(value)
        if (!response || response.ok !== true) {
          throw new Error(response && typeof response.error === "string"
            ? response.error
            : "Workflow host request failed")
        }
        return response.value
      }
      const host = async (method, value) => decode(await ${bridgeName}[method](JSON.stringify(copy(value))))
      const args = ${bridgeName}.missing ? undefined : JSON.parse(${bridgeName}.args)
      const agentReply = (value) => {
        if (value === null) return null
        if (!value || typeof value !== "object" || Array.isArray(value) || typeof value.text !== "string") {
          return value
        }
        const result = { text: value.text, data: copy(value.data) }
        if (typeof value.continuation !== "string") return result
        Object.defineProperties(result, {
          followup: {
            enumerable: false,
            value: async (input) => agentReply(await host("followup", {
              token: value.continuation,
              input: typeof input === "string" ? { prompt: input } : input,
            })),
          },
          close: {
            enumerable: false,
            value: async () => { await host("close", { token: value.continuation }) },
          },
        })
        return result
      }
      const agent = async (prompt, opts = {}) => {
        const input = typeof prompt === "string" ? { ...opts, prompt } : prompt
        if (!input || typeof input !== "object" || Array.isArray(input) || typeof input.prompt !== "string") {
          throw new TypeError("agent() expects a prompt string or input object")
        }
        if (!opts || typeof opts !== "object" || Array.isArray(opts)) {
          throw new TypeError("agent() options must be an object")
        }
        return agentReply(await host("agent", input))
      }
      const tool = async (name, toolArgs = {}, options = {}) => {
        if (typeof name !== "string" || !name) throw new TypeError("tool() expects a non-empty name")
        return copy(await host("tool", { name, args: toolArgs, options }))
      }
      const shell = async (command, opts = {}) => {
        if (typeof command !== "string" || !command) throw new TypeError("shell() expects a non-empty command")
        return copy(await host("shell", { command, opts }))
      }
      const workflow = async (name, workflowArgs = {}) => {
        if (typeof name !== "string" || !name) throw new TypeError("workflow() expects a non-empty name")
        return copy(await host("workflow", { name, args: workflowArgs }))
      }
      const parallel = async (tasks, options = {}) => {
        if (!Array.isArray(tasks) || tasks.some((task) => typeof task !== "function")) {
          throw new TypeError("parallel() expects an array of functions")
        }
        if (!options || typeof options !== "object" || Array.isArray(options)) {
          throw new TypeError("parallel() options must be an object")
        }
        if (tasks.length === 0) return []
        const requested = options.concurrencyLimit ?? tasks.length
        if (!Number.isFinite(requested) || requested < 1) {
          throw new TypeError("parallel() concurrencyLimit must be a positive number")
        }
        const result = new Array(tasks.length).fill(null)
        let index = 0
        const worker = async () => {
          while (index < tasks.length) {
            const current = index++
            try {
              result[current] = await tasks[current]()
            } catch (error) {
              log("Parallel task " + (current + 1) + " failed: " + error)
            }
          }
        }
        const limit = Math.min(16, tasks.length, Math.max(1, Math.floor(requested)))
        await Promise.all(Array.from({ length: limit }, () => worker()))
        // Preserve agent reply continuation methods for subsequent stages.
        // The outer workflow return still crosses a JSON boundary.
        return result
      }
      const pipeline = async (items, ...stages) => {
        if (!Array.isArray(items)) throw new TypeError("pipeline() expects an array as the first argument")
        const tail = stages.at(-1)
        const options = tail && typeof tail === "object" && typeof tail !== "function" ? stages.pop() : undefined
        if (!stages.length || stages.some((stage) => typeof stage !== "function")) {
          throw new TypeError("pipeline() stages must be functions")
        }
        return parallel(items.map((item, index) => async () => {
          let value = item
          for (const stage of stages) value = await stage(value, item, index)
          return value
        }), options)
      }
      const phase = (name) => {
        if (typeof name !== "string" || !name) throw new TypeError("phase() expects a non-empty string")
        decode(${bridgeName}.phase(JSON.stringify(name)))
      }
      const log = (...values) => decode(${bridgeName}.log(JSON.stringify(values.map((value) => {
        if (typeof value === "string") return value
        try { return JSON.stringify(value) } catch { return String(value) }
      }).join(" "))))
      const status = (value) => {
        if (typeof value === "string") value = { message: value }
        if (!value || typeof value !== "object" || Array.isArray(value)) {
          throw new TypeError("status() expects a string or object")
        }
        decode(${bridgeName}.status(JSON.stringify(copy(value))))
      }
      const setPhase = phase
      const setStatus = status
      const question = async () => { throw new Error("Workflow human questions are not supported in this runtime yet") }
      return copy(await (async function() {
        "use strict"
        ${body}
      }).call(undefined))
    })(${bridgeName})
  `
}

async function runPortable(start, args) {
  const body = portableBody(start.source)
  validatePortable(body)
  const vm = await import("node:vm")
  let agentCount = 0
  let activeAgents = 0
  const waitingAgents = []
  const acquire = () => new Promise((resolve) => {
    if (activeAgents < 16) {
      activeAgents++
      resolve()
    } else {
      waitingAgents.push(() => {
        activeAgents++
        resolve()
      })
    }
  })
  const release = () => {
    activeAgents--
    waitingAgents.shift()?.()
  }
  const response = async (operation) => {
    try {
      return JSON.stringify({ ok: true, value: await operation() })
    } catch (error) {
      return JSON.stringify({ ok: false, error: String(error) })
    }
  }
  const bridge = {
    args: args === undefined ? "" : JSON.stringify(args),
    missing: args === undefined,
    agent: (payload) => response(async () => {
      if (++agentCount > 1000) throw new Error("Portable workflow agent limit exceeded (1000)")
      const input = JSON.parse(payload)
      await acquire()
      try {
        return await request("agent", input)
      } finally {
        release()
      }
    }),
    followup: (payload) => response(() => request("agent.followup", JSON.parse(payload))),
    close: (payload) => response(() => request("agent.close", JSON.parse(payload))),
    tool: (payload) => response(() => request("tool", JSON.parse(payload))),
    shell: (payload) => response(() => request("shell", JSON.parse(payload))),
    workflow: (payload) => response(() => request("workflow", JSON.parse(payload))),
    phase: (payload) => JSON.stringify({ ok: true, value: notice("phase", JSON.parse(payload)) }),
    status: (payload) => JSON.stringify({ ok: true, value: notice("status", JSON.parse(payload)) }),
    log: (payload) => JSON.stringify({ ok: true, value: notice("log", JSON.parse(payload)) }),
  }
  const sandbox = Object.create(null)
  for (const name of portableBlocked) sandbox[name] = undefined
  const context = vm.createContext(sandbox, { codeGeneration: { strings: false, wasm: false } })
  new vm.Script(
    'Object.defineProperty(Math, "random", { value: undefined }); Object.freeze(Math)',
  ).runInContext(context)
  const bridgeName = "__haarness_portable_bridge"
  Object.defineProperty(context, bridgeName, { value: bridge, configurable: true })
  try {
    const script = new vm.Script(portableWrapper(body, bridgeName), {
      filename: start.sourcePath ?? "inline-portable.js",
    })
    return await Promise.resolve(script.runInContext(context, {
      timeout: Math.max(1, Math.min(Number(start.portableTimeoutMs), 2_147_483_647)),
    }))
  } finally {
    delete context[bridgeName]
  }
}

async function main() {
  const lines = inputLines()[Symbol.asyncIterator]()
  send({ type: "ready" })
  const first = await lines.next()
  if (first.done) throw new Error("Workflow host closed stdin before start")
  const start = parseHostFrame(first.value)
  if (
    start.type !== "start" ||
    typeof start.runId !== "string" ||
    start.runId.length === 0 ||
    typeof start.source !== "string" ||
    (start.dialect !== "native" && start.dialect !== "claude-portable") ||
    !Number.isSafeInteger(start.portableTimeoutMs) ||
    start.portableTimeoutMs < 1
  ) {
    throw new TypeError("Invalid workflow start frame")
  }
  const args = decodeValue(start.args)

  const pump = (async () => {
    for (;;) {
      const item = await lines.next()
      if (item.done) throw new Error("Workflow host closed stdin")
      const frame = parseHostFrame(item.value)
      if (frame.type !== "response" || !Number.isSafeInteger(frame.id) || typeof frame.ok !== "boolean") {
        throw new TypeError("Invalid workflow response frame")
      }
      const waiter = pending.get(frame.id)
      if (!waiter) continue
      pending.delete(frame.id)
      if (frame.ok) waiter.resolve(decodeValue(frame.value))
      else waiter.reject(new Error(typeof frame.error === "string" ? frame.error : "Workflow host request failed"))
    }
  })()

  const heartbeatInterval = start.dialect === "claude-portable"
    ? Math.max(5, Math.min(100, Math.ceil(start.portableTimeoutMs / 4)))
    : 5000
  const heartbeat = setInterval(() => send({ type: "heartbeat" }), heartbeatInterval)
  heartbeat.unref?.()
  try {
    const execution = start.dialect === "claude-portable"
      ? runPortable(start, args)
      : load(start.source, start.sourcePath, start.runId)
        .then((run) => run(args, context(start.runId)))
    const result = await Promise.race([
      execution,
      pump.then(() => {
        throw new Error("Workflow host input loop stopped")
      }),
    ])
    send({ type: "result", value: encodeValue(result) })
  } finally {
    clearInterval(heartbeat)
  }
}

main().catch((error) => {
  for (const waiter of pending.values()) waiter.reject(error)
  pending.clear()
  fail(error)
  process.exitCode = 1
})
