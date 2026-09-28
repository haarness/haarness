import asyncio,os
from uuid import uuid4
from haarness_sdk import HaarnessCloud,AsyncHaarnessCloud
with HaarnessCloud(os.environ['SDK_CLOUD_URL'],token=os.environ['SDK_CLOUD_TOKEN']) as c:
    host=c.register_remote_host({'id':str(uuid4()),'name':'SDK relay proof','mode':'host','metadata':{}})
    try:
        with c.connect_remote_host(host['id']) as socket:
            assert socket.recv(timeout=5)['type']=='ready'
            socket.send({'type':'catalog_changed'})
            assert c.get_remote_host(host['id'])['connected']
    finally:c.revoke_remote_host(host['id'])
print('Python sync Cloud connector: ready frame, JSON send and live host readback passed')
async def run():
    async with AsyncHaarnessCloud(os.environ['SDK_CLOUD_URL'],token=os.environ['SDK_CLOUD_TOKEN']) as c:
        host=await c.register_remote_host({'id':str(uuid4()),'name':'SDK relay proof','mode':'host','metadata':{}})
        try:
            async with await c.connect_remote_host(host['id']) as socket:
                assert (await asyncio.wait_for(socket.recv(),5))['type']=='ready'
                await socket.send({'type':'catalog_changed'})
                assert (await c.get_remote_host(host['id']))['connected']
        finally:await c.revoke_remote_host(host['id'])
    print('Python async Cloud connector: ready frame, JSON send and live host readback passed')
asyncio.run(run())
