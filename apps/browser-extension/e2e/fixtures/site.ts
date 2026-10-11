/**
 * The web the full-stack tests browse: one local HTTP server that Chrome
 * reaches under several names (`--host-resolver-rules` maps `*.test` to
 * 127.0.0.1), so one port gives distinct origins, `www.` twins and a
 * cross-site iframe.
 */
import { createServer, type IncomingMessage, type Server } from "node:http";

export interface Upload {
  name: string;
  body: string;
}

export interface TestSite {
  port: number;
  /** `http://<name>.test:<port>` */
  origin(name: string): string;
  url(name: string, path: string): string;
  /** Files the upload page posted, as the server received them. */
  uploads: Upload[];
  /** Requests the server saw (path and method), newest last. */
  requests: { host: string; path: string; method: string }[];
  close(): Promise<void>;
}

const page = (title: string, body: string, script = "") =>
  `<!doctype html><html><head><meta charset="utf-8"><title>${title}</title></head><body><h1>${title}</h1>${body}${script ? `<script>${script}</script>` : ""}</body></html>`;

function read(request: IncomingMessage) {
  return new Promise<string>((resolve) => {
    let body = "";
    request.setEncoding("utf8");
    request.on("data", (chunk: string) => (body += chunk));
    request.on("end", () => resolve(body));
  });
}

export async function startSite(): Promise<TestSite> {
  const uploads: Upload[] = [];
  const requests: TestSite["requests"] = [];
  let port = 0;
  const origin = (name: string) => `http://${name}.test:${port}`;
  const server: Server = createServer(async (request, response) => {
    const url = new URL(request.url ?? "/", "http://placeholder");
    const host = (request.headers.host ?? "").replace(/:\d+$/, "");
    requests.push({ host, path: url.pathname, method: request.method ?? "GET" });
    const html = (body: string) => {
      response.writeHead(200, { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" });
      response.end(body);
    };
    switch (url.pathname) {
      case "/":
        return html(page(`Home of ${host}`, `<p>Welcome to ${host}.</p><a href="/form">Form</a>`));
      case "/form":
        return html(
          page(
            "Order form",
            `<label>Name <input id="name" value="old value"></label>
             <label>Notes <textarea id="notes"></textarea></label>
             <label>Password <input id="password" type="password" value="hunter2"></label>
             <label>Size <select id="size"><option value="s">Small</option><option value="m">Medium</option><option value="l">Large</option></select></label>
             <label><input id="gift" type="checkbox"> Gift wrap</label>
             <button id="send" type="button">Send order</button>
             <button id="greet" type="button" onclick="alert('Hello from the page')">Greet</button>
             <button id="hover" type="button" onmouseover="this.textContent='Hovered'">Hover me</button>
             <p id="status">Waiting</p><p id="keys"></p>
             <div style="height:3000px"></div><p id="bottom">Bottom of the page</p>`,
            `const $ = (id) => document.getElementById(id);
             $("send").onclick = async () => {
               const order = { name: $("name").value, notes: $("notes").value, size: $("size").value, gift: $("gift").checked };
               console.log("order sent", JSON.stringify(order));
               const response = await fetch("/api/order", {
                 method: "POST",
                 headers: { "content-type": "application/json", "x-csrf-token": "csrf-secret", "authorization": "Bearer api-secret", "x-trace": "visible" },
                 body: JSON.stringify({ ...order, password: $("password").value }),
               });
               $("status").textContent = "Saved " + (await response.json()).id;
             };
             addEventListener("keydown", (event) => { $("keys").textContent += event.key; });
             addEventListener("scroll", () => { document.title = "Order form at " + Math.round(scrollY); });`,
          ),
        );
      case "/api/order": {
        const body = await read(request);
        response.writeHead(200, { "content-type": "application/json", "set-cookie": "session=cookie-secret", "x-request-token": "token-secret" });
        return response.end(JSON.stringify({ id: "order-1", received: body.length }));
      }
      case "/upload":
        return html(
          page(
            "Upload",
            `<input id="file" type="file"><p id="uploaded">Nothing yet</p>`,
            `const input = document.getElementById("file");
             input.onchange = async () => {
               const chosen = input.files[0];
               await fetch("/receive", { method: "POST", headers: { "x-file-name": encodeURIComponent(chosen.name) }, body: await chosen.text() });
               document.getElementById("uploaded").textContent = "Uploaded " + chosen.name;
             };`,
          ),
        );
      case "/receive": {
        uploads.push({ name: decodeURIComponent(String(request.headers["x-file-name"] ?? "")), body: await read(request) });
        response.writeHead(204);
        return response.end();
      }
      case "/downloads":
        return html(page("Downloads", `<a id="report" href="/report.txt">Get the report</a>`));
      case "/report.txt":
        response.writeHead(200, { "content-type": "text/plain", "content-disposition": 'attachment; filename="report.txt"' });
        return response.end("Quarterly report from the fixture");
      case "/redirect":
        response.writeHead(302, { location: url.searchParams.get("to") ?? "/" });
        return response.end();
      case "/popup":
        return html(page("Popup opener", `<button id="open" type="button" onclick="window.open('/popped', '_blank')">Open window</button>`));
      case "/popped":
        return html(page("Popped window", "<p>Opened by the page.</p>"));
      case "/frame":
        return html(page("Framed", `<iframe id="inner" src="${origin("other")}/inner" width="400" height="120"></iframe>`));
      case "/inner":
        return html(`<!doctype html><title>Inner</title><label>Inner field <input id="inner-field"></label><button type="button">Inner button</button>`);
      case "/later":
        return html(page("Later", `<p id="later">Loading</p>`, `setTimeout(() => (document.getElementById("later").textContent = "Ready now"), ${Number(url.searchParams.get("ms") ?? 1500)});`));
      case "/slow":
        await new Promise((resolve) => setTimeout(resolve, Number(url.searchParams.get("ms") ?? 3000)));
        return html(page("Slow page", "<p>Finally here.</p>"));
      default:
        response.writeHead(404, { "content-type": "text/plain" });
        return response.end("Not found");
    }
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  port = typeof address === "object" && address ? address.port : 0;
  return {
    port,
    origin,
    url: (name, path) => origin(name) + path,
    uploads,
    requests,
    async close() {
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    },
  };
}
