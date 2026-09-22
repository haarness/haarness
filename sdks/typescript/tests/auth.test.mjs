import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { Haarness, authorizationHeaders } from "../dist/index.js";

test("server credentials encode UTF-8, default usernames, and reject ambiguous usernames", () => {
  assert.deepEqual(authorizationHeaders({type:"none"}), {});
  assert.deepEqual(authorizationHeaders("legacy"), {Authorization:"Bearer legacy"});
  assert.deepEqual(authorizationHeaders({type:"basic", username:"", password:"日本:secret"}), {
    Authorization: "Basic " + Buffer.from("haarness:日本:secret").toString("base64"),
  });
  assert.throws(() => authorizationHeaders({type:"basic",username:"bad:user",password:"secret"}));
  assert.throws(() => authorizationHeaders("bad\r\nheader"));
});

test("explicit anonymous and password connections override ambient bearer credentials", async () => {
  const requests=[];
  const server=createServer((request,response)=>{
    requests.push(request.headers.authorization);
    response.setHeader("Content-Type","application/json");
    response.end(JSON.stringify({name:"haarness"}));
  }).listen(0,"127.0.0.1");
  await once(server,"listening");
  const baseUrl=`http://127.0.0.1:${server.address().port}`;
  const original=process.env.HAARNESS_SERVER_TOKEN;
  process.env.HAARNESS_SERVER_TOKEN="ambient-private-token";
  try {
    for(const options of [
      {auth:{type:"none"}},
      {username:"alex",password:"secret"},
      {auth:{type:"basic",username:"alex",password:"secret"}},
      {token:"legacy"},
    ]) {
      const client=new Haarness({baseUrl,...options});
      try {await client.info();} finally {client.close();}
    }
    assert.deepEqual(requests,[undefined,"Basic YWxleDpzZWNyZXQ=","Basic YWxleDpzZWNyZXQ=","Bearer legacy"]);
  } finally {
    if(original===undefined)delete process.env.HAARNESS_SERVER_TOKEN;else process.env.HAARNESS_SERVER_TOKEN=original;
    server.closeAllConnections();
    await new Promise(resolve=>server.close(resolve));
  }
});
