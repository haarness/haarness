// Drives the installed AppImage through a self-update (scripts/test-linux-update.sh): a stand-in release service on
// loopback offers "9.9.9", whose file is another release's AppImage on the release domain, with the size and SHA-256
// the release service published for it.
import { createRequire } from "node:module";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
const require = createRequire("/work/node_modules/");
const { _electron: electron } = require("/work/node_modules/playwright");

const home = process.env.HOME;
const target = join(home, ".local/bin/Haarness.AppImage");
const launcher = join(home, ".local/bin/haarness-desktop");
const out = "/work/out";
const payload = JSON.parse(readFileSync("/work/out/payload.json", "utf8"));
const log = (...args) => console.log(new Date().toISOString().slice(11, 19), ...args);
const sha = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const requests = [];

const service = createServer((request, response) => {
  requests.push(request.url);
  if (request.url === "/api/v1/releases/stable/desktop") {
    const body = { channel: "stable", manifest: { version: "9.9.9", products: { desktop: { version: "9.9.9", artifacts: [{ ...payload, file: "Haarness-9.9.9-linux-arm64.AppImage" }] } } } };
    response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(body));
    return;
  }
  response.writeHead(404).end();
});
await new Promise((resolve) => service.listen(0, "127.0.0.1", resolve));
const releaseUrl = `http://127.0.0.1:${service.address().port}`;
const data = join(home, "haarness-test");
const env = {
  ...process.env,
  HAARNESS_DESKTOP_DATA: join(data, "desktop"),
  HAARNESS_HOME: join(data, "home"),
  HAARNESS_DATA_DIR: join(data, "state"),
  HAARNESS_TELEMETRY: "0",
  HAARNESS_RELEASE_URL: releaseUrl,
};
const before = sha(target);
log("installed AppImage", before.slice(0, 16));

async function open() {
  const app = await electron.launch({ executablePath: launcher, args: ["--no-sandbox"], env, timeout: 180_000 });
  const page = await app.firstWindow();
  await app.evaluate(({ BrowserWindow }) => {
    const win = BrowserWindow.getAllWindows()[0];
    win.setContentSize(1280, 860);
    win.show();
  });
  await page.locator(".sidebar-footer").waitFor({ timeout: 120_000 });
  const welcome = page.getByRole("dialog", { name: "Welcome to Haarness" });
  if (await welcome.waitFor({ timeout: 8000 }).then(() => true, () => false)) await page.keyboard.press("Escape");
  return { app, page };
}

const { app, page } = await open();
const facts = await app.evaluate(({ app }) => ({ version: app.getVersion(), packaged: app.isPackaged, appimage: process.env.APPIMAGE, extract: process.env.APPIMAGE_EXTRACT_AND_RUN, pid: process.pid, argv: process.argv }));
log("running", JSON.stringify(facts));
await page.getByRole("button", { name: "Settings" }).first().click();
const settings = page.getByRole("dialog", { name: "Settings", exact: true });
await settings.getByRole("button", { name: "About", exact: true }).click();
const updates = settings.locator('[data-setting="Updates"]');
await updates.getByRole("button", { name: "Check for updates" }).click();
let last = "";
for (let i = 0; i < 900; i++) {
  const text = (await updates.innerText()).replace(/\s+/g, " ");
  if (text !== last && !/Downloading [1-9]?[1-9]%/.test(text)) log("About:", text);
  last = text;
  if (/ready to install|failed|damaged|couldn|signature/i.test(text)) break;
  await sleep(1000);
}
await page.screenshot({ path: join(out, "linux-about-ready.png") });
if (!last.includes("Haarness 9.9.9 is ready to install")) throw new Error("not ready: " + last);
const staged = join(home, ".local/bin/.Haarness.AppImage.haarness-update");
log("staged beside the app:", existsSync(staged), existsSync(staged) && sha(staged) === payload.digest.sha256 ? "(sha matches the release)" : "");
await page.keyboard.press("Escape");
const row = page.locator(".sidebar-footer .update-row");
log("sidebar:", await row.innerText());
await page.locator(".sidebar-footer").screenshot({ path: join(out, "linux-sidebar-ready.png") });
const exited = new Promise((resolve) => app.process().once("exit", resolve));
await row.click({ noWaitAfter: true }).catch(() => {});
await exited;
log("old app exited");
for (let i = 0; i < 120 && sha(target) !== payload.digest.sha256; i++) await sleep(500);
const after = sha(target);
log("AppImage after:", after.slice(0, 16), after === payload.digest.sha256 ? "= the release's file" : "UNCHANGED");
const result = join(data, "desktop/updates/install-result.json");
log("install result:", existsSync(result) ? readFileSync(result, "utf8").trim() : "(already read by the reopened app)");
log("left beside the app:", readdirSync(join(home, ".local/bin")).join(" "));
// The helper reopens the app; it must still be running after a while (with --no-sandbox, which this container needs).
let reopened = "";
for (let i = 0; i < 60 && !reopened; i++) {
  reopened = execFileSync("sh", ["-c", "ps -eo pid,args | grep -E 'haarness-desktop( |$)|Haarness.AppImage' | grep -v -E 'grep|haarness serve|bin/haarness ' || true"], { encoding: "utf8" }).trim();
  if (!reopened) await sleep(500);
}
log("reopened:", reopened || "nothing");
await sleep(10_000);
const alive = execFileSync("sh", ["-c", "ps -eo pid,args | grep -E '/tmp/appimage_extracted_[^ ]*/haarness-desktop' | grep -v grep | head -3 || true"], { encoding: "utf8" }).trim();
log("still running after 10 s:", alive || "nothing");
// Quit it, then open the installed app again: it is the new file.
execFileSync("sh", ["-c", "pkill -TERM -f '[a]ppimage_extracted_.*/haarness-desktop' || true; sleep 3"]);
const again = await open();
const version = await again.app.evaluate(({ app }) => app.getVersion());
log("installed app now runs version", version);
await again.page.screenshot({ path: join(out, "linux-after.png") });
await again.app.close();
writeFileSync(join(out, "linux-requests.txt"), requests.join("\n"));
const passed = version === payload.version && after === payload.digest.sha256 && !!alive;
log(passed ? "PASS: replaced in place, reopened with its flags, runs the new file" : "FAIL");
service.close();
process.exit(passed ? 0 : 1);
