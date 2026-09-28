import asyncio, os
from haarness_sdk import HaarnessRouter, AsyncHaarnessRouter
request={'model':'openai/gpt-4.1-nano','messages':[{'role':'user','content':'Say exactly SDK_RELEASE_OK'}],'max_tokens':32}
urls=[os.environ['SDK_ROUTER_URL'],os.environ['SDK_NAMED_ROUTER_URL']]
for url in urls:
    with HaarnessRouter(url,token=os.environ['HAARNESS_API_KEY']) as c:
        assert c.models()['data']
        assert c.chat_completions(request)['choices']
        assert 'SDK_RELEASE_OK' in ''.join(x.get('choices',[{}])[0].get('delta',{}).get('content','') for x in c.stream_chat_completions(request) if x.get('choices'))
print('Python sync router: default + named models, JSON inference and SSE passed')
async def run():
    for url in urls:
        async with AsyncHaarnessRouter(url,token=os.environ['HAARNESS_API_KEY']) as c:
            assert (await c.models())['data']
            assert (await c.chat_completions(request))['choices']
            text=''
            async for x in c.stream_chat_completions(request):
                if x.get('choices'):text+=x['choices'][0].get('delta',{}).get('content','')
            assert 'SDK_RELEASE_OK' in text,text
    print('Python async router: default + named models, JSON inference and SSE passed')
asyncio.run(run())
