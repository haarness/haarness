"""Live Cloud release SDK proof. Restore the previous preview publication in finally."""
import asyncio, hashlib, os
from datetime import datetime, timezone
from uuid import uuid4
from haarness_sdk import HaarnessCloud, AsyncHaarnessCloud

def values():
    body=b'PYTHON_RELEASE_PROOF';version='sdk.'+str(uuid4())
    options={'headers':{'x-haarness-internal-token':os.environ['HAARNESS_RELEASE_TOKEN']}}
    query=dict(version=version,product='cli',file='sdk-test',kind='binary',os='linux',arch='x64')
    upload_options={'headers':{**options['headers'],'x-content-sha256':hashlib.sha256(body).hexdigest(),'content-length':str(len(body))}}
    return body,version,options,query,upload_options

def publication(version,id):return dict(channel='preview',version=version,sourceRevision='1'*40,releasedAt=datetime.now(timezone.utc).isoformat(),artifacts=[id])
def sync():
    body,version,options,query,upload_options=values()
    with HaarnessCloud(os.environ['RELEASE_TEST_URL'],token='') as c:
        old=next((r for r in c.release_history({'channel':'preview'}) if r['current']),None);published=None
        try:
            a=c.upload_release_artifact(body,query,options=upload_options)
            published=c.publish_release(publication(version,a['id']),options=options)
            assert c.current_release({'channel':'preview'})['manifest']['version']==version
            assert c.download_release_artifact(a['id'])==body
            assert c.download_release('preview','cli',dict(os='linux',arch='x64',version=version))==body
            assert 'clear_quarantine' in c.release_install_sh()
            assert 'Unblock-File' in c.release_install_ps1()
            print('Python sync: full release SDK lifecycle passed')
        finally:
            if published:c.withdraw_release(published['id'],options=options)
            if old:c.promote_release(old['id'],options=options)
async def asynchronous():
    body,version,options,query,upload_options=values()
    async with AsyncHaarnessCloud(os.environ['RELEASE_TEST_URL'],token='') as c:
        old=next((r for r in await c.release_history({'channel':'preview'}) if r['current']),None);published=None
        try:
            a=await c.upload_release_artifact(body,query,options=upload_options)
            published=await c.publish_release(publication(version,a['id']),options=options)
            assert (await c.current_release({'channel':'preview'}))['manifest']['version']==version
            assert await c.download_release_artifact(a['id'])==body
            assert await c.download_release('preview','cli',dict(os='linux',arch='x64',version=version))==body
            assert 'clear_quarantine' in await c.release_install_sh()
            assert 'Unblock-File' in await c.release_install_ps1()
            print('Python async: full release SDK lifecycle passed')
        finally:
            if published:await c.withdraw_release(published['id'],options=options)
            if old:await c.promote_release(old['id'],options=options)
sync();asyncio.run(asynchronous())
