"""OpenAI-compatible inference on default and named Cloud router URLs (/v1)."""
from __future__ import annotations
import json, os
from typing import Any, Iterator, AsyncIterator
from urllib.parse import urlsplit
from .transport import Transport, AsyncTransport, RequestOptions, TransportError, decode
from .types import JsonValue
from typing_extensions import TypedDict

class RouterChatRequest(TypedDict):
    model: str
    messages: list[JsonValue]

def origin(url: str) -> str:
    u=urlsplit(url)
    if (u.scheme!='https' and not (u.scheme=='http' and u.hostname in ('localhost','127.0.0.1','::1'))) or u.username or u.password or u.query or u.fragment:
        raise ValueError('Router requires HTTPS or loopback HTTP without credentials, query or fragment')
    return url

def event(data: list[str]) -> dict[str, Any] | None:
    value='\n'.join(data)
    if value=='[DONE]': return None
    try: result=json.loads(value)
    except ValueError as error: raise TransportError('Invalid router event JSON') from error
    if not isinstance(result,dict): raise TransportError('Invalid router event object')
    if result.get('error'): raise TransportError('Router streaming error: '+json.dumps(result['error']))
    return result

class HaarnessRouter(Transport):
    def __init__(self,base_url: str, *, token: str | None = None, timeout: float = 30):
        super().__init__(origin(base_url),token=token if token is not None else os.getenv('HAARNESS_API_KEY',''),timeout=timeout)
    def models(self, *, options: RequestOptions | None = None) -> dict[str,Any]: return self.request('GET','/models',options=options)
    def chat_completions(self, body: dict[str,Any] | RouterChatRequest, *, options: RequestOptions | None = None) -> dict[str,Any]: return self.request('POST','/chat/completions',body={**body,'stream':False},options=options)
    def stream_chat_completions(self, body: dict[str,Any] | RouterChatRequest, *, options: RequestOptions | None = None) -> Iterator[dict[str,Any]]:
        with self._http.stream('POST',self.base_url+'/chat/completions',**self._kwargs({**body,'stream':True},None,False,options)) as response:
            if not response.is_success: response.read();decode(response)
            data=[];size=0
            for line in response.iter_lines():
                size+=len(line)
                if size>8*1024*1024: raise TransportError('Router event exceeds 8 MiB')
                if line.startswith('data:'): data.append(line[5:].lstrip())
                elif not line and data:
                    value=event(data);data=[];size=0
                    if value is None: return
                    yield value
            raise TransportError('Router stream ended before [DONE]')

class AsyncHaarnessRouter(AsyncTransport):
    def __init__(self,base_url: str, *, token: str | None = None, timeout: float = 30):
        super().__init__(origin(base_url),token=token if token is not None else os.getenv('HAARNESS_API_KEY',''),timeout=timeout)
    async def models(self, *, options: RequestOptions | None = None) -> dict[str,Any]: return await self.request('GET','/models',options=options)
    async def chat_completions(self, body: dict[str,Any] | RouterChatRequest, *, options: RequestOptions | None = None) -> dict[str,Any]: return await self.request('POST','/chat/completions',body={**body,'stream':False},options=options)
    async def stream_chat_completions(self, body: dict[str,Any] | RouterChatRequest, *, options: RequestOptions | None = None) -> AsyncIterator[dict[str,Any]]:
        async with self._http.stream('POST',self.base_url+'/chat/completions',**self._kwargs({**body,'stream':True},None,False,options)) as response:
            if not response.is_success: await response.aread();decode(response)
            data=[];size=0
            async for line in response.aiter_lines():
                size+=len(line)
                if size>8*1024*1024: raise TransportError('Router event exceeds 8 MiB')
                if line.startswith('data:'): data.append(line[5:].lstrip())
                elif not line and data:
                    value=event(data);data=[];size=0
                    if value is None: return
                    yield value
            raise TransportError('Router stream ended before [DONE]')
