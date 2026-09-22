#!/usr/bin/env python3
"""Minimal newline-delimited MCP stdio server for product-boundary smoke tests."""

import json
import sys


def reply(request_id, result):
    print(
        json.dumps(
            {"jsonrpc": "2.0", "id": request_id, "result": result},
            separators=(",", ":"),
        ),
        flush=True,
    )


for line in sys.stdin:
    try:
        request = json.loads(line)
    except json.JSONDecodeError:
        continue
    request_id = request.get("id")
    method = request.get("method")
    if request_id is None:
        continue
    if method == "initialize":
        reply(
            request_id,
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
                "serverInfo": {"name": "ecosystem-smoke", "version": "1.0.0"},
            },
        )
    elif method == "tools/list":
        reply(
            request_id,
            {
                "tools": [
                    {
                        "name": "echo",
                        "description": "Return a deterministic smoke marker",
                        "inputSchema": {
                            "type": "object",
                            "properties": {"message": {"type": "string"}},
                            "required": ["message"],
                        },
                    }
                ]
            },
        )
    elif method == "tools/call":
        message = request.get("params", {}).get("arguments", {}).get("message", "")
        reply(
            request_id,
            {
                "content": [{"type": "text", "text": f"MCP_ECHO_OK:{message}"}],
                "isError": False,
            },
        )
    elif method == "resources/list":
        reply(
            request_id,
            {
                "resources": [
                    {
                        "uri": "smoke://fixture/readme",
                        "name": "Smoke fixture resource",
                        "mimeType": "text/plain",
                    }
                ]
            },
        )
    elif method == "resources/templates/list":
        reply(
            request_id,
            {
                "resourceTemplates": [
                    {
                        "uriTemplate": "smoke://fixture/{name}",
                        "name": "Smoke fixture template",
                    }
                ]
            },
        )
    elif method == "resources/read":
        uri = request.get("params", {}).get("uri", "")
        reply(
            request_id,
            {
                "contents": [
                    {
                        "uri": uri,
                        "mimeType": "text/plain",
                        "text": "MCP_RESOURCE_OK",
                    }
                ]
            },
        )
    elif method == "prompts/list":
        reply(
            request_id,
            {
                "prompts": [
                    {
                        "name": "review",
                        "description": "Deterministic fixture prompt",
                        "arguments": [
                            {"name": "target", "description": "Review target", "required": True}
                        ],
                    }
                ]
            },
        )
    elif method == "prompts/get":
        target = request.get("params", {}).get("arguments", {}).get("target", "")
        reply(
            request_id,
            {
                "description": "Resolved fixture prompt",
                "messages": [
                    {
                        "role": "user",
                        "content": {"type": "text", "text": f"MCP_PROMPT_OK:{target}"},
                    }
                ],
            },
        )
    else:
        print(
            json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {"code": -32601, "message": "method not found"},
                },
                separators=(",", ":"),
            ),
            flush=True,
        )
