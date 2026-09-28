import asyncio, httpx, pytest
from haarness_sdk import HaarnessRouter,AsyncHaarnessRouter,TransportError
request={'model':'test','messages':[{'role':'user','content':'hello'}]}
@pytest.mark.parametrize('payload,valid',[
 ('data: {"choices":[{"delta":{"content":"日本語"}}]}\r\n\r\ndata: [DONE]\r\n\r\n',True),
 ('data: {broken}\n\n',False),('data: {"choices":[]}\n\n',False),('data: {"error":{"message":"failed"}}\n\n',False)])
def test_streams_fail_closed_without_replay(payload,valid):
    calls=[]
    def handler(r):
        calls.append(r);assert str(r.url)=='http://localhost/routers/test/v1/chat/completions'
        return httpx.Response(200,text=payload)
    with HaarnessRouter('http://localhost/routers/test/v1',token='test') as client:
        client._http.close();client._http=httpx.Client(transport=httpx.MockTransport(handler))
        if valid: assert list(client.stream_chat_completions(request))[0]['choices'][0]['delta']['content']=='日本語'
        else:
            with pytest.raises(TransportError):list(client.stream_chat_completions(request))
    assert len(calls)==1
    calls.clear()
    async def run():
        async with AsyncHaarnessRouter('http://localhost/routers/test/v1',token='test') as client:
            await client._http.aclose();client._http=httpx.AsyncClient(transport=httpx.MockTransport(handler))
            if valid: assert [x async for x in client.stream_chat_completions(request)][0]['choices']
            else:
                with pytest.raises(TransportError):[x async for x in client.stream_chat_completions(request)]
    asyncio.run(run());assert len(calls)==1
