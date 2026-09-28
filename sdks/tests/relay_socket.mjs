import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {HaarnessCloud} from '../typescript/dist/index.js';
const c=new HaarnessCloud({baseUrl:process.env.SDK_CLOUD_URL,token:process.env.SDK_CLOUD_TOKEN});
const host=await c.registerRemoteHost({id:randomUUID(),name:'SDK relay proof',mode:'host',metadata:{}});
let socket;
try {
  socket=await c.connectRemoteHost(host.id);
  const iterator=socket[Symbol.asyncIterator]();
  assert.equal((await iterator.next()).value.type,'ready');
  socket.send({type:'catalog_changed'});
  assert.equal((await c.getRemoteHost(host.id)).connected,true);
  console.log('TypeScript Cloud connector: ready frame, JSON send and live host readback passed');
} finally {socket?.close();await c.revokeRemoteHost(host.id);c.close();}
