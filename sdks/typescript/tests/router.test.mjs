import {test} from 'node:test';
import assert from 'node:assert/strict';
import {HaarnessRouter,TransportError} from '../dist/index.js';
function router(text, status=200) {
  let calls=0;
  const client=new HaarnessRouter({baseUrl:'http://127.0.0.1:1234/routers/test/v1',token:'test',fetch:async(url,init)=>{
    calls++;assert.match(url,/\/routers\/test\/v1\/chat\/completions$/);assert.equal(init.redirect,'error');assert.equal(init.headers.Authorization,'Bearer test');assert.equal(JSON.parse(init.body).stream,true);
    return new Response(new ReadableStream({start(controller){const bytes=new TextEncoder().encode(text);for(const byte of bytes)controller.enqueue(Uint8Array.of(byte));controller.close();}}),{status});
  }});
  return {client,calls:()=>calls};
}
const request={model:'test',messages:[{role:'user',content:'hello'}]};
test('router SSE handles split UTF-8 and CRLF, DONE and exact named URL',async()=>{
 const {client,calls}=router(': keepalive\r\ndata: {"choices":[{"delta":{"content":"日本語"}}]}\r\n\r\ndata: [DONE]\r\n\r\n');
 try {const chunks=[];for await(const chunk of client.streamChatCompletions(request))chunks.push(chunk);assert.equal(chunks[0].choices[0].delta.content,'日本語');assert.equal(calls(),1);}finally{client.close();}
});
for(const text of ['data: {broken}\n\n','data: {"choices":[]}\n\n','data: {"error":{"message":"failed"}}\n\n']) {
 test('router rejects invalid, truncated or failed streams without replay: '+text,async()=>{
  const {client,calls}=router(text);try{await assert.rejects(async()=>{for await(const _ of client.streamChatCompletions(request)){}},TransportError);assert.equal(calls(),1);}finally{client.close();}
 });
}
test('router rejects insecure remote origins and credential-bearing URLs',()=>{
 for(const baseUrl of ['http://example.com/v1','https://secret@example.com/v1','https://example.com/v1?key=secret'])assert.throws(()=>new HaarnessRouter({baseUrl}));
});
