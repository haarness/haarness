#!/usr/bin/env python3
import json
import sys
import os
import time


def receive():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, _, value = line.decode("ascii").partition(":")
        if name.lower() == "content-length":
            length = int(value.strip())
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))


def send(message):
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii"))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


def location(uri):
    return {
        "uri": uri,
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 4},
        },
    }


def call_item(uri):
    return {
        "name": "mockFunction",
        "kind": 12,
        "uri": uri,
        "range": location(uri)["range"],
        "selectionRange": location(uri)["range"],
    }


documents = {}
while True:
    message = receive()
    if message is None:
        break
    method = message.get("method")
    request_id = message.get("id")
    params = message.get("params") or {}
    if request_id is not None:
        time.sleep(float(os.environ.get("INIT_DELAY" if method == "initialize" else "REQUEST_DELAY", "0")))
    if method == "initialize":
        send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "result": {
                    "capabilities": {
                        "textDocumentSync": 1,
                        "definitionProvider": True,
                        "referencesProvider": True,
                        "hoverProvider": True,
                        "documentSymbolProvider": True,
                        "workspaceSymbolProvider": True,
                        "implementationProvider": True,
                        "callHierarchyProvider": True,
                    }
                },
            }
        )
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": request_id, "result": None})
    elif method == "exit":
        break
    elif method in ("textDocument/didOpen", "textDocument/didChange"):
        document = params["textDocument"]
        uri = document["uri"]
        text = (
            document.get("text")
            if method == "textDocument/didOpen"
            else params["contentChanges"][-1]["text"]
        )
        documents[uri] = text
        diagnostics = []
        if "BROKEN" in text:
            diagnostics.append(
                {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 6},
                    },
                    "severity": 1,
                    "code": "mock-error",
                    "source": "haarness-mock",
                    "message": "mock syntax error",
                }
            )
        send(
            {
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": {"uri": uri, "diagnostics": diagnostics},
            }
        )
    elif request_id is not None:
        uri = (params.get("textDocument") or {}).get("uri")
        if uri is None:
            uri = next(iter(documents), "file:///mock")
        if method in ("textDocument/definition", "textDocument/implementation"):
            result = location(uri)
        elif method == "textDocument/references":
            result = [location(uri)]
        elif method == "textDocument/hover":
            result = {"contents": {"kind": "markdown", "value": "mock hover"}}
        elif method == "textDocument/documentSymbol":
            result = [
                {
                    "name": "mockFunction",
                    "kind": 12,
                    "range": location(uri)["range"],
                    "selectionRange": location(uri)["range"],
                }
            ]
        elif method == "workspace/symbol":
            result = [dict(call_item(uri), location=location(uri))]
        elif method == "textDocument/prepareCallHierarchy":
            result = [call_item(uri)]
        elif method == "callHierarchy/incomingCalls":
            result = [{"from": call_item(uri), "fromRanges": [location(uri)["range"]]}]
        elif method == "callHierarchy/outgoingCalls":
            result = [{"to": call_item(uri), "fromRanges": [location(uri)["range"]]}]
        else:
            result = None
        send({"jsonrpc": "2.0", "id": request_id, "result": result})
