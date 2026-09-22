import readline from "node:readline";
const { Haarness, ApiError, attachments } = await import(
  process.env.SDK_MODULE ?? "../typescript/dist/index.js"
);
const client = new Haarness({
  baseUrl: process.env.SDK_URL,
  token: process.env.SDK_TOKEN,
  timeoutMs: 40000,
});
const camel = (name) => name.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
try {
  for await (const line of readline.createInterface({ input: process.stdin })) {
    const command = JSON.parse(line),
      name = command.name,
      args = command.args ?? [];
    try {
      if (name === "upload_profile") args[0] = Buffer.from(args[0], "base64");
      let value;
      if (name === "attachments") value = await attachments(args[0]);
      else if (name === "events") {
        value = [];
        const abort = new AbortController();
        const timer = setTimeout(() => abort.abort(), 5000);
        try {
          for await (const event of client.events({
            after: args[0],
            signal: abort.signal,
          })) {
            value.push(event);
            if (value.length >= args[1]) break;
          }
        } finally {
          clearTimeout(timer);
          abort.abort();
        }
      } else if (name === "connect_pty") {
        const abort = new AbortController();
        const timer = setTimeout(() => abort.abort(), 5000);
        const socket = await client.connectPty(
          args[0],
          { cursor: args[1] },
          { signal: abort.signal },
        );
        let output = Buffer.alloc(0);
        const controls = [];
        try {
          socket.send("printf 'SDK_PTY_OK\\n'\n");
          for await (const message of socket) {
            if (message instanceof Uint8Array)
              output = Buffer.concat([output, message]);
            else controls.push(message);
            if (output.includes("SDK_PTY_OK\r\n") && socket.cursor > 0) break;
          }
          value = { text: output.toString(), cursor: socket.cursor, controls };
        } finally {
          clearTimeout(timer);
          socket.close();
        }
      } else if (name === "bad_auth") {
        const other = new Haarness({
          baseUrl: process.env.SDK_URL,
          token: "incorrect",
        });
        try {
          value = await other.info();
        } finally {
          other.close();
        }
      } else {
        if (command.kwargs?.options)
          args.push({ timeoutMs: command.kwargs.options.timeout * 1000 });
        value = await client[camel(name)](...args);
      }
      if (value instanceof Uint8Array)
        value = { base64: Buffer.from(value).toString("base64") };
      console.log(JSON.stringify({ ok: true, value: value ?? null }));
    } catch (error) {
      console.log(
        JSON.stringify(
          error instanceof ApiError
            ? {
                ok: false,
                status: error.status,
                code: error.code,
                retryable: error.retryable,
                message: error.message,
              }
            : { ok: false, exception: error.name, message: error.message },
        ),
      );
    }
  }
} finally {
  client.close();
}
