import assert from 'node:assert/strict';
import { randomUUID, createHash } from 'node:crypto';
import {HaarnessCloud} from '../typescript/dist/index.js';
const client=new HaarnessCloud({baseUrl:process.env.RELEASE_TEST_URL,token:''});
const options={headers:{'x-haarness-internal-token':process.env.HAARNESS_RELEASE_TOKEN}};
const old=(await client.releaseHistory({channel:'preview'})).find(r=>r.current);
let published;
try {
  const version='sdk.'+randomUUID(), body=Buffer.from('SDK_RELEASE_PROOF');
  const sha256=createHash('sha256').update(body).digest('hex');
  const artifact=await client.uploadReleaseArtifact(body,{version,product:'cli',file:'sdk-test',kind:'binary',os:'linux',arch:'x64'},{headers:{...options.headers,'x-content-sha256':sha256,'content-length':String(body.length)}});
  published=await client.publishRelease({channel:'preview',version,sourceRevision:'1'.repeat(40),releasedAt:new Date().toISOString(),artifacts:[artifact.id]},options);
  assert.equal((await client.currentRelease({channel:'preview'})).manifest.version,version);
  assert.deepEqual(Buffer.from(await client.downloadReleaseArtifact(artifact.id)),body);
  assert.deepEqual(Buffer.from(await client.downloadRelease('preview','cli',{os:'linux',arch:'x64',version})),body);
  assert.match(await client.releaseInstallSh(),/clear_quarantine/);
  assert.match(await client.releaseInstallPs1(),/Unblock-File/);
  console.log('TypeScript: upload, publish, history, selector, artifact bytes, installers passed');
} finally {
  if(published) await client.withdrawRelease(published.id,options);
  if(old) await client.promoteRelease(old.id,options);
  client.close();
}
