import assert from 'node:assert/strict';
import {HaarnessCloud,ApiError} from '../typescript/dist/index.js';
const cloud=new HaarnessCloud({baseUrl:process.env.HAARNESS_CLOUD_API_URL});
try {
  const context=await cloud.apiKeyContext();assert.ok(context.key.scopes.includes('workspaces:agent'));
  await cloud.listAgentSessions();await cloud.getWallet();
  const session=cloud.session(process.env.HAARNESS_KEY_E2E_SESSION);
  const runtime=await session.connect();
  assert.equal((await runtime.readFile({path:'unified-key-proof.txt'})).content,'UNIFIED_SHARED_FILES_OK');
  const chat=await session.chats.create({title:'TypeScript key'});
  assert.equal((await session.chats.list()).length,3);
  await assert.rejects(cloud.listAccountApiKeys(),e=>e instanceof ApiError && e.status===403);
  await chat.archive();
  console.log('PASS: TypeScript SDK key context, billing, sessions, chat, shared files and scope denial');
}finally{cloud.close();}
