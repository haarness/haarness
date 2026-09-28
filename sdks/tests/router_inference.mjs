import assert from 'node:assert/strict';
import {HaarnessRouter, HaarnessCloud} from '../typescript/dist/index.js';
const cloud=new HaarnessCloud({baseUrl:process.env.SDK_CLOUD_URL,token:process.env.HAARNESS_API_KEY});
const selected=await cloud.router(process.env.SDK_ROUTER_ID,process.env.HAARNESS_API_KEY);
const standard=new HaarnessRouter({baseUrl:process.env.SDK_ROUTER_URL,token:process.env.HAARNESS_API_KEY});
try {
  for (const router of [standard,selected]) {
    assert.ok((await router.models()).data.length);
    const request={model:'openai/gpt-4.1-nano',messages:[{role:'user',content:'Say exactly SDK_RELEASE_OK'}],max_tokens:32};
    assert.ok((await router.chatCompletions(request)).choices.length);
    let text='';for await (const chunk of router.streamChatCompletions(request))text+=chunk.choices?.[0]?.delta?.content??'';
    assert.ok(text.includes('SDK_RELEASE_OK'),text);
  }
  console.log('TypeScript router: default + named models, JSON inference and SSE passed');
} finally {standard.close();selected.close();cloud.close();}
