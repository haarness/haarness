import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, readFile, realpath, symlink } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stopChatHome } from "./fixtures/chat-home";
import { isolatedEnv } from "./fixtures/isolated-env";
let app: ElectronApplication, page: Page, root: string, main: string, linked: string, current: string;
let env: Record<string,string>; const binary=resolve(process.env.HAARNESS_BIN??"../../target/debug/haarness");
const errors:string[]=[];
test.describe.configure({mode:"serial"});
async function choose(path:string) {
  await app.evaluate(({dialog},path)=>{dialog.showOpenDialog=async()=>({canceled:false,filePaths:[path]});},path);
  await page.getByRole("button",{name:"Open another project",exact:true}).click();
  current=path;
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready","true",{timeout:60000});
  await expect(page.locator(".titlebar")).toContainText(path.split("/").at(-1)!);
}
async function api(method:string,...args:any[]):Promise<any> {return page.evaluate(({connection,method,args})=>window.haarness.api(connection,method,args),{connection:"local:"+current,method,args});}
async function session() {await page.getByRole("button",{name:"New chat",exact:true}).click();await expect(page.getByRole("textbox",{name:"Message",exact:true})).toBeVisible();}
async function send(text:string) {const box=page.getByRole("textbox",{name:"Message",exact:true});await box.fill(text.startsWith("/")?text:" "+text);await page.getByRole("button",{name:/Send message|Send guidance/}).click();}
async function idle() {await expect.poll(async()=>(await api("activeTurns")).length,{timeout:30000}).toBe(0);}
async function proof(name:string) {await page.screenshot({path:test.info().outputPath(name+".png")});await test.info().attach(name,{body:await page.locator("body").innerText(),contentType:"text/plain"});}
test.beforeAll(async()=>{
  test.setTimeout(120000);root=await realpath(await mkdtemp(join(tmpdir(),"haarness-permission-parity-ui-")));
  main=join(root,"primary");linked=join(root,"linked");
  await mkdir(join(main,".haarness/commands"),{recursive:true});
  await writeFile(join(main,".haarness/config.toml"),'[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n[execution]\nshell_timeout_ms=0\nsubagent_concurrency=1\nsubagent_depth=2\n');
  await writeFile(join(main,".haarness/commands/context.md"),'---\ndescription: Read dynamic context\n---\nContext: !`printf DYNAMIC_APPROVAL_PROOF`\n');
  await writeFile(join(main,".env"),"PARITY_SECRET=fixture-only");
  await symlink(".env",join(main,"notes.txt"));
  const git=(args:string[])=>execFileSync("git",args,{cwd:main,stdio:"ignore"});
  git(["init","-b","main"]);git(["add","."]);git(["-c","user.name=Fixture","-c","user.email=fixture@example.test","commit","-m","fixture"]);git(["worktree","add","-b","linked",linked]);
  env=isolatedEnv();
  Object.assign(env,{HAARNESS_BIN:binary,HAARNESS_HOME:join(root,"home"),HAARNESS_DATA_DIR:join(root,"data"),HAARNESS_DESKTOP_DATA:join(root,"desktop"),HAARNESS_TEST_PROVIDER:"1",HAARNESS_DESKTOP_TEST:"1"});
  delete env.ELECTRON_RUN_AS_NODE;delete env.ELECTRON_RENDERER_URL;
  app=await electron.launch({args:["."],env,timeout:60000});page=await app.firstWindow();page.setDefaultTimeout(15000);
  page.on("pageerror",error=>errors.push(error.message));
  await page.context().tracing.start({screenshots:true,snapshots:true,sources:true});await choose(main);
});
test.afterEach(async()=>{if(page) await page.screenshot({path:test.info().outputPath("final.png")}).catch(()=>{});});
test.afterAll(async()=>{
  if(page) await page.context().tracing.stop({path:test.info().outputPath("parity-trace.zip")}).catch(()=>{});
  if(app){await app.evaluate(({BrowserWindow})=>{for(const win of BrowserWindow.getAllWindows())win.destroy();}).catch(()=>{});await app.close().catch(()=>{});}
  for(const workspace of [main,linked])try{execFileSync(binary,["--workspace",workspace,"serve","--stop"],{env,stdio:"ignore",timeout:30000});}catch{}
  stopChatHome(binary, env);
  await test.info().attach("fixture",{body:root??"not started",contentType:"text/plain"});
});
test("repository approval is explicit, survives reload, and follows linked worktrees",async()=>{
  await session();await send('!tool bash {"command":"git status --short"}');
  // Repository scope lives under the Always allow menu, beside the folder scope.
  await page.getByRole("button",{name:"Always allow",exact:true}).click();
  const repository=page.getByRole("menuitem",{name:"Always for this repository",exact:true});
  await expect(repository).toBeVisible();await proof("repository-approval");
  await repository.click();await idle();
  await page.reload();await expect(page.locator(".application")).toHaveAttribute("data-project-ready","true");
  await choose(linked);
  await page.getByRole("button",{name:"Project options for primary",exact:true}).click();
  await page.getByRole("menuitem",{name:"Enable worktree view",exact:true}).click();
  await session();await send('!tool bash {"command":"git status --porcelain"}');
  await expect.poll(async()=>(await api("listThreads")).length).toBeGreaterThan(0);
  // The chat exists before its turn does, so "no active turn" can come too
  // early; wait for the command's result instead.
  const threads=await api("listThreads");
  await expect.poll(async()=>(await api("listItems",threads[0].id)).some((i:any)=>i.data.type==="tool_result"&&i.data.is_error===false),{timeout:30000}).toBe(true);
  await idle();
  await expect(page.getByRole("button",{name:"Allow once",exact:true})).toHaveCount(0);
  const items=await api("listItems",threads[0].id);
  expect(items.some((i:any)=>i.data.type==="permission_request")).toBe(false);
  await proof("worktree-without-repeat-prompt");
});
test("dynamic context requests approval in the UI and a denial prevents execution",async()=>{
  await choose(main);await session();
  await send("/context");
  await expect(page.getByRole("button",{name:"Allow once",exact:true})).toBeVisible();await proof("dynamic-context-approval");
  await page.getByRole("button",{name:"Allow once",exact:true}).click();await idle();
  await expect(page.locator(".transcript")).toContainText("DYNAMIC_APPROVAL_PROOF");
  await send('!tool bash {"command":"printf SHOULD_NOT_RUN > denied.txt"}');
  await expect(page.getByRole("button",{name:"Deny",exact:true})).toBeVisible();await page.getByRole("button",{name:"Deny",exact:true}).click();await idle();
  await expect(readFile(join(main,"denied.txt"),"utf8")).rejects.toThrow();
});
test("nested agents complete with one slot and secret reads still require approval",async()=>{
  await session();
  const leaf='!tool bash {"command":"printf NESTED_UI_PROOF > nested-ui.txt"}';
  const child="!tool task "+JSON.stringify({prompt:leaf,agent:"general"});
  await send("!tool task "+JSON.stringify({prompt:child,agent:"general"}));
  await expect(page.getByRole("button",{name:"Allow once",exact:true})).toBeVisible();await proof("nested-child-approval");
  await page.getByRole("button",{name:"Allow once",exact:true}).click();await idle();
  expect(await readFile(join(main,"nested-ui.txt"),"utf8")).toBe("NESTED_UI_PROOF");
  await send('!tool read {"path":".env"}');await expect(page.getByRole("button",{name:"Allow once",exact:true})).toBeVisible();
  await proof("secret-still-requires-approval");await page.getByRole("button",{name:"Deny",exact:true}).click();await idle();
  // A link to the secret asks about the file it leads to, and the card names it.
  await send('!tool read {"path":"notes.txt"}');await expect(page.getByRole("button",{name:"Allow once",exact:true})).toBeVisible();
  await expect(page.locator(".approval-target")).toHaveText("Leads to .env");
  await proof("linked-secret-names-its-target");await page.getByRole("button",{name:"Deny",exact:true}).click();await idle();expect(errors).toEqual([]);
});

test("larger media survives a real drop, draft reload and server submission",async()=>{
  await session();
  const transfer=await page.evaluateHandle(()=>{
    const bytes=new Uint8Array(11*1024*1024);
    const png=atob("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=");
    bytes.set(Uint8Array.from(png,c=>c.charCodeAt(0)));
    const data=new DataTransfer();data.items.add(new File([bytes],"large-parity.png",{type:"image/png"}));return data;
  });
  await page.getByRole("region",{name:"Chat",exact:true}).dispatchEvent("drop",{dataTransfer:transfer});
  await expect(page.getByRole("button",{name:"Remove large-parity.png",exact:true})).toBeVisible();
  await page.reload();
  await expect(page.getByRole("button",{name:"Remove large-parity.png",exact:true})).toBeVisible();
  await proof("larger-media-draft");
  await send("MEDIA_TRANSPORT_PROOF");await expect(page.locator(".transcript")).toContainText("Echo: MEDIA_TRANSPORT_PROOF");await idle();
  expect(await page.locator(".composer-attachments").count()).toBe(0);
  const threads=await api("listThreads");
  const items=await api("listItems",threads[0].id);
  const message=items.find((item:any)=>item.data.type==="user_message"&&item.data.text.includes("MEDIA_TRANSPORT_PROOF"));
  // The server keeps the image every provider takes: the 11 MB file holds one pixel.
  expect(message.data.attachments[0]).toMatch(/^data:image\/png;name=large-parity\.png;base64,/);
  expect(message.data.attachments[0].length).toBeLessThan(1024);
  await transfer.dispose();expect(errors).toEqual([]);
});
