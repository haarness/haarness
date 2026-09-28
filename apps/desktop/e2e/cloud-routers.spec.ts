import {test,expect,_electron as electron} from "@playwright/test";
import {execFileSync} from "node:child_process";
import {mkdtemp,mkdir,rm} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join,resolve} from "node:path";

// Opt in to a local Cloud account with a funded default and configured BYOK router.
test("Desktop switches Cloud routers live with correct model prices and credit settlement",async()=>{
  const api=process.env.HAARNESS_ACCOUNT_E2E_URL,byokId=process.env.HAARNESS_ROUTER_TEST_ID;
  test.skip(!api||!byokId||!process.env.HAARNESS_API_KEY,"Provide the local router fixture and disposable model/billing API key");
  test.setTimeout(240000);expect(new URL(api!).hostname).toBe("127.0.0.1");
  const root=await mkdtemp(join(tmpdir(),"haarness-routers-desktop-"));
  const workspace=join(root,"project");await mkdir(workspace);execFileSync("git",["init","-q",workspace]);
  const env:Record<string,string>={...Object.fromEntries(Object.entries(process.env).filter((e):e is [string,string]=>e[1]!==undefined)),HAARNESS_BIN:resolve(process.env.HAARNESS_BIN??"../../target/debug/haarness"),HAARNESS_DATA_DIR:join(root,"data"),HAARNESS_CLOUD_CONFIG_DIR:join(root,"account"),HAARNESS_DESKTOP_DATA:join(root,"desktop"),HAARNESS_CLOUD_API_URL:api!,HAARNESS_DESKTOP_TEST:"1"};delete env.ELECTRON_RUN_AS_NODE;
  const app=await electron.launch({args:["."],env});
  try {
    const page=await app.firstWindow();
    const status=await page.evaluate(()=>window.haarness.account({action:"status"}));
    const routers=await page.evaluate(()=>window.haarness.account({action:"routers"}));
    const byok=routers.items.find((r:any)=>r.id===byokId),standard=routers.items.find((r:any)=>r.isDefault);
    expect(byok.credentialMode).toBe("byok");
    const connection=await page.evaluate(directory=>window.haarness.connect({workspace:directory,trust:false}),workspace);
    for(const row of [byok,standard]) {
      await page.getByRole("button",{name:status.user.displayName,exact:true}).click();
      const dialog=page.getByRole("dialog",{name:"Haarness account",exact:true});
      const select=dialog.getByRole("combobox",{name:"Cloud router",exact:true});
      await expect(select).toBeVisible();await select.selectOption(row.id);
      await expect.poll(()=>page.evaluate(()=>window.haarness.account({action:"status"}))).toMatchObject({selectedRouterId:row.id});
      await expect(dialog.getByRole("alert")).toHaveCount(0);
      await page.screenshot({path:test.info().outputPath(`router-${row.credentialMode}.png`)});
      await dialog.getByRole("button",{name:"Done",exact:true}).click();
      const providers=await page.evaluate(id=>window.haarness.api<any[]>(id!,"refreshProviders"),connection!.id);
      const models=providers.find(p=>p.id==="haarness")!.models;
      if(row.credentialMode==="byok") {
        expect(models.map((m:any)=>m.id)).toEqual(["openai/gpt-4.1-nano"]);
        expect(models[0].pricing).toMatchObject({input:0,output:0});
      }
      const before=await page.evaluate(()=>window.haarness.account({action:"billing"}));
      const marker=`DESKTOP_ROUTER_${row.credentialMode.toUpperCase()}_OK`;
      const thread=await page.evaluate(async({id,marker})=>{
        const thread=await window.haarness.api<any>(id,"createThread",[{title:marker,provider:"haarness",model:"openai/gpt-4.1-nano"}]);
        await window.haarness.api<any>(id,"startTurn",[thread.id,{requestId:crypto.randomUUID(),text:`Without tools, reply with exactly ${marker}`}]);return thread.id;
      },{id:connection!.id,marker});
      await expect.poll(()=>page.evaluate(async({id,thread,marker})=>{
        const items=await window.haarness.api<any[]>(id,"listItems",[thread]);return items.some(i=>i.data?.type==="assistant_message"&&i.data.text.includes(marker));
      },{id:connection!.id,thread,marker}),{timeout:120000}).toBe(true);
      await expect.poll(()=>page.evaluate(async()=>{const wallet=await window.haarness.account({action:"billing"});return wallet.heldReserve.nanos;})).toBe("0");
      const after=await page.evaluate(()=>window.haarness.account({action:"billing"}));
      if(row.credentialMode==="byok"){expect(after.includedAi).toEqual(before.includedAi);expect(after.purchased).toEqual(before.purchased);}
      else await expect.poll(async()=>BigInt((await page.evaluate(()=>window.haarness.account({action:"billing"}))).includedAi.nanos)<BigInt(before.includedAi.nanos)).toBe(true);
    }
  } finally {
    await app.close();execFileSync(env.HAARNESS_BIN,["--workspace",workspace,"serve","--stop"],{env,stdio:"ignore"});await rm(root,{recursive:true,force:true});
  }
});
