"""Minimal deterministic MCP peer over real newline-delimited stdio JSON-RPC."""

import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    params = request.get("params", {})
    if method == "initialize":
        result = {
            "protocolVersion": params["protocolVersion"],
            "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
            "serverInfo": {"name": "sdk-fixture", "version": "1"},
        }
    elif method == "tools/list":
        result = {
            "tools": [
                {
                    "name": "echo",
                    "description": "Echo fixture input",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"text": {"type": "string"}},
                        "required": ["text"],
                    },
                }
            ]
        }
    elif method == "tools/call":
        result = {
            "content": [
                {"type": "text", "text": params.get("arguments", {}).get("text", "")}
            ],
            "isError": False,
        }
    elif method == "resources/list":
        result = {
            "resources": [
                {"uri": "fixture://readme", "name": "fixture", "mimeType": "text/plain"}
            ]
        }
    elif method == "resources/templates/list":
        result = {
            "resourceTemplates": [
                {"uriTemplate": "fixture://{name}", "name": "template"}
            ]
        }
    elif method == "resources/read":
        result = {
            "contents": [
                {
                    "uri": params["uri"],
                    "text": "MCP_RESOURCE_OK",
                    "mimeType": "text/plain",
                }
            ]
        }
    elif method == "prompts/list":
        result = {
            "prompts": [
                {
                    "name": "review",
                    "description": "Fixture review",
                    "arguments": [{"name": "target", "required": False}],
                }
            ]
        }
    elif method == "prompts/get":
        result = {
            "messages": [
                {
                    "role": "user",
                    "content": {
                        "type": "text",
                        "text": "Review "
                        + params.get("arguments", {}).get("target", "fixture"),
                    },
                }
            ]
        }
    elif method == "ping":
        result = {}
    else:
        print(
            json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "error": {"code": -32601, "message": "Unknown method"},
                }
            ),
            flush=True,
        )
        continue
    print(
        json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}),
        flush=True,
    )
