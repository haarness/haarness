"""Execute scenarios through the installed sync or async Python SDK."""

import asyncio
import base64
import json
import os
import sys
from haarness_sdk import Haarness, AsyncHaarness, ApiError, attachments


async def main():
    asynchronous = sys.argv[1] == "async"
    client = (AsyncHaarness if asynchronous else Haarness)(
        os.environ["SDK_URL"], token=os.environ["SDK_TOKEN"], timeout=40
    )

    async def call(name, args, kwargs=None):
        value = getattr(client, name)(*args, **(kwargs or {}))
        return await value if asynchronous else value

    try:
        for line in sys.stdin:
            command = json.loads(line)
            name = command["name"]
            args = command.get("args", [])
            try:
                if name == "upload_profile":
                    args[0] = base64.b64decode(args[0])
                if name == "attachments":
                    value = attachments(args[0])
                elif name == "events":
                    events = client.events(after=args[0])
                    value = []
                    try:
                        if asynchronous:
                            async for event in events:
                                value.append(event)
                                if len(value) >= args[1]:
                                    break
                        else:
                            for event in events:
                                value.append(event)
                                if len(value) >= args[1]:
                                    break
                    finally:
                        if asynchronous:
                            await events.aclose()
                        else:
                            events.close()
                elif name == "connect_pty":
                    socket = (
                        await client.connect_pty(args[0], cursor=args[1])
                        if asynchronous
                        else client.connect_pty(args[0], cursor=args[1])
                    )
                    output = b""
                    controls = []
                    try:
                        if asynchronous:
                            await socket.send("printf 'SDK_PTY_OK\\n'\n")
                        else:
                            socket.send("printf 'SDK_PTY_OK\\n'\n")
                        for _ in range(100):
                            message = (
                                await asyncio.wait_for(socket.recv(), 5)
                                if asynchronous
                                else socket.recv(timeout=5)
                            )
                            if isinstance(message, bytes):
                                output += message
                            else:
                                controls.append(message)
                            if b"SDK_PTY_OK\r\n" in output and socket.cursor > 0:
                                break
                        value = {
                            "text": output.decode(errors="replace"),
                            "cursor": socket.cursor,
                            "controls": controls,
                        }
                    finally:
                        if asynchronous:
                            await socket.close()
                        else:
                            socket.close()
                elif name == "bad_auth":
                    other = Haarness(os.environ["SDK_URL"], token="incorrect")
                    try:
                        other.info()
                    finally:
                        other.close()
                else:
                    value = await call(name, args, command.get("kwargs"))
                if isinstance(value, bytes):
                    value = {"base64": base64.b64encode(value).decode()}
                print(json.dumps({"ok": True, "value": value}), flush=True)
            except ApiError as error:
                print(
                    json.dumps(
                        {
                            "ok": False,
                            "status": error.status,
                            "code": error.code,
                            "retryable": error.retryable,
                            "message": str(error),
                        }
                    ),
                    flush=True,
                )
            except Exception as error:
                print(
                    json.dumps(
                        {
                            "ok": False,
                            "exception": type(error).__name__,
                            "message": str(error),
                        }
                    ),
                    flush=True,
                )
    finally:
        if asynchronous:
            await client.close()
        else:
            client.close()


asyncio.run(main())
