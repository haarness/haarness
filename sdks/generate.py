#!/usr/bin/env python3
"""Generate both SDKs from the server's schemas and registered Rust routes.

Refresh schema.json from a freshly built server's /openapi.json, then run this.
--check fails on drift without modifying files. No network or generator service.
"""

import argparse
import json
import re
from pathlib import Path
from inventory import routes

BASE = Path(__file__).resolve().parent


def ref(name):
    return {"$ref": "#/components/schemas/" + name}


def obj(fields, required=()):
    return {"type": "object", "properties": fields, "required": list(required)}


def arr(value):
    return {"type": "array", "items": value}


S = {"type": "string"}
N = {"type": "integer"}
B = {"type": "boolean"}
J = {}


def nullable(value):
    return {"anyOf": [value, {"type": "null"}]}


def camel(value):
    first, *rest = value.split("_")
    return first + "".join(x.title() for x in rest)


def rust(value):
    value = re.sub(r"\b\w+::", "", value.strip())
    for wrapper in ("Vec", "Option"):
        if value.startswith(wrapper + "<"):
            inner = rust(value[len(wrapper) + 1 : -1])
            return arr(inner) if wrapper == "Vec" else nullable(inner)
    return {
        "String": S,
        "str": S,
        "Value": J,
        "bool": B,
        "u64": N,
        "usize": N,
        "u32": N,
    }.get(value, ref(value))


def generic(text, word):
    m = re.search(r"\b" + word + r"<", text)
    if not m:
        return None
    i = m.end()
    start = i
    depth = 1
    while depth:
        depth += (text[i] == "<") - (text[i] == ">")
        i += 1
    return text[start : i - 1]


def specification():
    doc = json.loads((BASE / "schema.json").read_text())
    schemas = doc["components"]["schemas"]
    schemas.update(
        {
            "SessionArchive": obj(
                {
                    "format": S,
                    "schemaVersion": N,
                    "sourceVersion": S,
                    "exportedAt": S,
                    "rootThreadId": S,
                    "sanitized": B,
                    "threads": arr(ref("SessionArchiveThread")),
                },
                (
                    "format",
                    "schemaVersion",
                    "sourceVersion",
                    "exportedAt",
                    "rootThreadId",
                    "threads",
                ),
            ),
            "SessionArchiveThread": obj(
                {
                    "thread": ref("Thread"),
                    "turns": arr(ref("Turn")),
                    "items": arr(ref("Item")),
                    "goal": ref("GoalState"),
                    "loopConfig": ref("LoopConfig"),
                },
                ("thread", "turns", "items"),
            ),
            "SessionArchiveImportRequest": obj(
                {"archive": ref("SessionArchive")}, ("archive",)
            ),
            "SessionArchiveImportResult": obj(
                {
                    "root": ref("Thread"),
                    "threads": arr(ref("Thread")),
                    "created": arr(S),
                    "merged": arr(S),
                },
                ("root", "threads", "created", "merged"),
            ),
            "Record": obj(
                {
                    "id": S,
                    "name": S,
                    "scope": {"enum": ["global", "local", "both"]},
                    "createdAt": S,
                    "uploadedAt": S,
                    "entryCount": N,
                    "sizeBytes": N,
                    "sha256": S,
                },
                (
                    "id",
                    "name",
                    "scope",
                    "createdAt",
                    "uploadedAt",
                    "entryCount",
                    "sizeBytes",
                    "sha256",
                ),
            ),
            "LanguageStatus": obj(
                {"id": S, "extensions": arr(S), "status": S, "error": nullable(S)},
                ("id", "extensions", "status", "error"),
            ),
            "FormatterStatus": obj(
                {"id": S, "extensions": arr(S), "status": S},
                ("id", "extensions", "status"),
            ),
            "ToolDefinition": obj(
                {"name": S, "description": S, "input_schema": J},
                ("name", "description", "input_schema"),
            ),
            "ToolOutput": obj({"text": S, "metadata": J}, ("text", "metadata")),
            "SkillContent": obj(
                {"name": S, "instructions": S}, ("name", "instructions")
            ),
            "GoalRequest": {
                "anyOf": [ref("GoalActionRequest"), obj({"goal": nullable(S)})]
            },
            "LoopRequest": obj(
                {"prompt": nullable(S), "intervalSeconds": nullable(N), "stop": B}
            ),
            "LoopResponse": obj({"loop": nullable(ref("LoopConfig"))}, ("loop",)),
            "ShellRequest": obj({"command": S}, ("command",)),
            "RevertRequest": obj({"sequence": nullable(N)}),
            "UsageResponse": obj(
                {
                    "inputTokens": N,
                    "outputTokens": N,
                    "cacheReadTokens": N,
                    "cacheWriteTokens": N,
                    "costUsd": nullable({"type": "number"}),
                    "costIncomplete": B,
                    "latestInputTokens": N,
                    "descendants": J,
                },
                (
                    "inputTokens",
                    "outputTokens",
                    "cacheReadTokens",
                    "cacheWriteTokens",
                    "costUsd",
                ),
            ),
            "InterruptResponse": obj(
                {"threadId": S, "workflows": N, "turnsAndTasks": N},
                ("threadId", "workflows", "turnsAndTasks"),
            ),
            "HealthResponse": obj({"status": S}, ("status",)),
            "ResourceReadRequest": obj({"uri": S}, ("uri",)),
        }
    )
    schemas["ChatOptions"] = {
        **schemas["StartTurnRequest"],
        "properties": {
            k: v
            for k, v in schemas["StartTurnRequest"]["properties"].items()
            if k != "text"
        },
        "required": [],
    }
    queries = {
        "AnalyticsQuery": obj(
            {"days": N, "project": S, "currentProject": B, "limit": N}
        ),
        "ToolCatalogQuery": obj({"threadId": S}),
        "PtyConnectQuery": obj({"cursor": N}),
        "WorkflowRunsQuery": obj({"threadId": S, "limit": N}),
        "WorkflowRunQuery": obj({"afterSequence": N}),
        "WorkflowWaitQuery": obj({"timeoutMs": N}),
        "ThreadQuery": obj({"archived": B}),
        "SessionArchiveQuery": obj({"sanitize": B}),
        "ItemsQuery": obj({"after": N}),
        "DiffReviewQuery": obj({"summary": B, "path": S}),
        "VcsDiffQuery": obj(
            {
                "mode": {"enum": ["working", "branch", "worktree", "git"]},
                "context": N,
                "summary": B,
                "path": S,
            }
        ),
        "FileSearchQuery": obj({"q": S, "limit": N}, ("q",)),
        "FileQuery": obj({"path": S, "hidden": B}),
        "Browse": obj({"path": S, "prefix": S, "hidden": B, "limit": N, "after": S}),
        "BackgroundQuery": obj({"childThreadId": S}),
        "WaitQuery": obj({"timeout_ms": N}),
        "Filters": obj({"query": S, "offset": N, "limit": N}),
    }
    schemas.update(queries)
    overrides = {
        "load_skill": "SkillContent",
        "get_loop": "LoopResponse",
        "set_loop": "LoopResponse",
        "thread_usage": "UsageResponse",
        "interrupt_thread_tree": "InterruptResponse",
        "health": "HealthResponse",
        "host_projects": "WorkspaceListResponse",
        "host_directories": "DirectoryListing",
        "host_open": "WorkspaceInfo",
    }
    operations = []
    doc["paths"] = {}
    for name, method, path, sig, file in routes():
        before, after = sig.split("->", 1)
        body = generic(before, "Json")
        result = generic(after, "Json")
        query = generic(before, "Query")
        operation = {
            "name": name,
            "method": method,
            "path": path,
            "pathParams": re.findall(r"\{(\w+)\}", path),
            "response": rust(result) if result else {"type": "null"},
        }
        if name in overrides:
            operation["response"] = ref(overrides[name])
        if body:
            operation["body"] = rust(body)
        if name == "read_mcp_resource":
            operation["body"] = ref("ResourceReadRequest")
        if query:
            query = (
                "AnalyticsQuery"
                if query.endswith("::Options")
                else query.split("::")[-1]
            )
            operation["query"] = ref(query)
        if name in ("download_profile", "upload_profile"):
            operation[
                "binaryResponse" if name == "download_profile" else "binaryBody"
            ] = True
        if name == "connect_pty":
            operation["stream"] = "websocket"
        if name == "events":
            operation["stream"] = "sse"
        operations.append(operation)
        entry = {
            "operationId": name,
            "responses": {
                "200": {
                    "description": "Success",
                    "content": {"application/json": {"schema": operation["response"]}},
                }
            },
            "parameters": [],
        }
        for param in operation["pathParams"]:
            entry["parameters"].append(
                {"name": param, "in": "path", "required": True, "schema": S}
            )
        if query:
            schema = queries[query.split("::")[-1]]
            for key, value in schema["properties"].items():
                entry["parameters"].append(
                    {
                        "name": key,
                        "in": "query",
                        "required": key in schema.get("required", []),
                        "schema": value,
                    }
                )
        if body:
            entry["requestBody"] = {
                "required": True,
                "content": {"application/json": {"schema": operation["body"]}},
            }
        if operation.get("binaryBody"):
            entry["requestBody"] = {
                "required": True,
                "content": {
                    "application/zip": {
                        "schema": {"type": "string", "format": "binary"}
                    }
                },
            }
        if operation.get("binaryResponse"):
            entry["responses"] = {
                "200": {
                    "description": "Profile archive",
                    "content": {
                        "application/zip": {
                            "schema": {"type": "string", "format": "binary"}
                        }
                    },
                }
            }
        if (
            operation["response"] == {"type": "null"}
            and not operation.get("binaryResponse")
            and not operation.get("stream")
        ):
            entry["responses"] = {
                "202" if name in ("run_shell", "shutdown_server") else "204": {
                    "description": "Accepted" if name == "run_shell" else "No content"
                }
            }
        if operation.get("stream"):
            entry["responses"] = {
                "101" if name == "connect_pty" else "200": {
                    "description": operation["stream"] + " stream"
                }
            }
        status = {
            "create_thread": "201",
            "fork_thread": "201",
            "create_pty": "201",
            "upload_profile": "201",
            "start_turn": "202",
            "queue_input": "202",
        }.get(name)
        if status:
            entry["responses"][status] = entry["responses"].pop("200")
        if name in ("execute_command", "start_workflow"):
            entry["responses"]["202"] = entry["responses"]["200"]
        if path.startswith("/v1/"):
            entry["security"] = [{"bearer": []}]
            entry["responses"]["401"] = {
                "description": "Bearer authentication required"
            }
        doc["paths"].setdefault(path, {})[method.lower()] = entry
    for old, new in [("Record", "ProfileRecord"), ("ApiError", "ApiErrorBody")]:
        schemas[new] = schemas.pop(old)
        doc = json.loads(
            json.dumps(doc).replace("/schemas/" + old + '"', "/schemas/" + new + '"')
        )
        operations = json.loads(
            json.dumps(operations).replace(
                "/schemas/" + old + '"', "/schemas/" + new + '"'
            )
        )
        schemas = doc["components"]["schemas"]
    # Ensure every reference is actually available rather than silently emitting any.
    for name in re.findall(
        r'"\$ref": "#/components/schemas/([^" ]+)"', json.dumps([operations, schemas])
    ):
        if name not in schemas:
            raise ValueError("Missing schema: " + name)
    doc["components"]["securitySchemes"] = {
        "bearer": {"type": "http", "scheme": "bearer"}
    }
    return doc, operations


def ts(schema):
    if "$ref" in schema:
        return schema["$ref"].rsplit("/", 1)[-1]
    for union, join in [("oneOf", " | "), ("anyOf", " | "), ("allOf", " & ")]:
        if union in schema:
            return "(" + join.join(ts(x) for x in schema[union]) + ")"
    if "enum" in schema:
        return " | ".join(json.dumps(x) for x in schema["enum"])
    kind = schema.get("type")
    if isinstance(kind, list):
        return "(" + " | ".join(ts({**schema, "type": x}) for x in kind) + ")"
    if kind == "array":
        return "Array<" + ts(schema.get("items", {})) + ">"
    if kind == "object" or "properties" in schema:
        fields = [
            json.dumps(k)
            + ("" if k in schema.get("required", []) else "?")
            + ": "
            + ts(v)
            for k, v in schema.get("properties", {}).items()
        ]
        extra = schema.get("additionalProperties")
        if extra:
            fields.append(
                "[key: string]: " + ("JsonValue" if extra is True else ts(extra))
            )
        return (
            "{ " + "; ".join(fields) + " }"
            if fields
            else "{ [key: string]: JsonValue }"
        )
    return {
        "string": "string",
        "number": "number",
        "integer": "number",
        "boolean": "boolean",
        "null": "null",
    }.get(kind, "JsonValue")


class PythonTypes:
    def __init__(self):
        self.lines = []
        self.used = set()

    def type(self, schema, name):
        if "$ref" in schema:
            return repr(schema["$ref"].rsplit("/", 1)[-1])
        for union in ("oneOf", "anyOf", "allOf"):
            if union in schema:
                # allOf is primarily nullable refs emitted by utoipa; merge objects if present.
                parts = schema[union]
                if union == "allOf" and all("properties" in x for x in parts):
                    return self.type(
                        obj(
                            {k: v for x in parts for k, v in x["properties"].items()},
                            [k for x in parts for k in x.get("required", [])],
                        ),
                        name,
                    )
                return (
                    "Union["
                    + ", ".join(
                        self.type(x, name + str(i)) for i, x in enumerate(parts)
                    )
                    + "]"
                )
        if "enum" in schema:
            return "Literal[" + ", ".join(repr(x) for x in schema["enum"]) + "]"
        kind = schema.get("type")
        if isinstance(kind, list):
            return (
                "Union["
                + ", ".join(
                    self.type({**schema, "type": x}, name + str(i))
                    for i, x in enumerate(kind)
                )
                + "]"
            )
        if kind == "array":
            return "List[" + self.type(schema.get("items", {}), name + "Item") + "]"
        if kind == "object" or "properties" in schema:
            props = schema.get("properties", {})
            if not props:
                extra = schema.get("additionalProperties")
                return (
                    "Dict[str, "
                    + (
                        self.type(extra, name + "Value")
                        if isinstance(extra, dict)
                        else "JsonValue"
                    )
                    + "]"
                )
            if name in self.used:
                raise ValueError("duplicate " + name)
            self.used.add(name)
            fields = []
            for key, value in props.items():
                value = self.type(
                    value, name + "".join(x.title() for x in re.split(r"[_-]", key))
                )
                fields.append(
                    repr(key)
                    + ": "
                    + (
                        "Required"
                        if key in schema.get("required", [])
                        else "NotRequired"
                    )
                    + "["
                    + value
                    + "]"
                )
            self.lines.append(
                name + " = TypedDict(" + repr(name) + ", {" + ", ".join(fields) + "})"
            )
            return repr(name)
        return {
            "string": "str",
            "number": "float",
            "integer": "int",
            "boolean": "bool",
            "null": "None",
        }.get(kind, "JsonValue")


def generate(doc=None, ops=None):
    if doc is None:
        doc, ops = specification()
    schemas = doc["components"]["schemas"]
    outputs = {}
    outputs["openapi.json"] = json.dumps(doc, indent=2, sort_keys=True) + "\n"
    outputs["operations.json"] = json.dumps(ops, indent=2) + "\n"
    outputs["typescript/src/types.ts"] = (
        "// Generated by sdks/generate.py. Do not edit.\nexport type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };\n"
        + "".join(
            "export type " + name + " = " + ts(schema) + ";\n"
            for name, schema in sorted(schemas.items())
        )
    )
    p = PythonTypes()
    for name, schema in sorted(schemas.items()):
        value = p.type(schema, name)
        if value != repr(name):
            p.lines.append(
                name
                + ": TypeAlias = "
                + ("Union[" + value + "]" if value.startswith("'") else value)
            )
    outputs["python/src/haarness_sdk/types.py"] = (
        '"""Generated wire types. Keys preserve the server JSON spelling."""\nfrom typing import Dict, List, Union, Literal, TypeAlias\nfrom typing_extensions import TypedDict, Required, NotRequired\nJsonValue: TypeAlias = Union[None, bool, int, float, str, List["JsonValue"], Dict[str, "JsonValue"]]\n'
        + "\n".join(p.lines)
        + "\n"
    )
    tsclient = [
        "// Generated by sdks/generate.py. Do not edit.",
        'import { Transport, type RequestOptions } from "./transport.js";',
        'import type * as T from "./types.js";',
        "export class Api extends Transport {",
    ]
    pyclients = {
        False: [
            '"""Generated endpoint methods. Do not edit."""',
            "from typing import cast",
            "from . import types as T",
            "from .transport import Transport, AsyncTransport, RequestOptions",
        ],
        True: [],
    }
    for asynchronous in (False, True):
        pyclients[asynchronous].append(
            "class "
            + ("AsyncApi(AsyncTransport)" if asynchronous else "Api(Transport)")
            + ":"
        )
    docs = [
        "# SDK operation inventory",
        "",
        "| Python | TypeScript | HTTP |",
        "| --- | --- | --- |",
    ]
    for op in ops:
        name = op["name"]
        tname = camel(name)
        docs.append(f"| `{name}` | `{tname}` | `{op['method']} {op['path']}` |")
        if op.get("stream"):
            continue
        params = op["pathParams"]
        pyargs = [x + ": str" for x in params]
        tsargs = [x + ": string" for x in params]

        def tst(value):
            return re.sub(
                r"\b([A-Z]\w+)\b",
                lambda m: m[0] if m[0] in ("Array", "Record") else "T." + m[0],
                ts(value),
            )

        def pyt(value):
            tmp = PythonTypes()
            v = tmp.type(value, "Unused")
            if tmp.lines:
                raise ValueError("inline signature requires named schema " + name)
            return (
                re.sub(r"'([A-Za-z]\w*)'", r"T.\1", v)
                .replace("List[", "list[")
                .replace("Dict[", "dict[")
                .replace("JsonValue", "T.JsonValue")
                .replace("Union[", "Union[")
            )

        # All operation annotations are references or containers, except JsonValue.
        if op.get("binaryBody"):
            pyargs += ["body: bytes"]
            tsargs += ["body: Uint8Array"]
        elif "body" in op:
            pyargs += ["body: " + pyt(op["body"])]
            tsargs += ["body: " + tst(op["body"])]
        if "query" in op:
            pyargs += [
                "query: "
                + pyt(op["query"])
                + (
                    ""
                    if schemas[op["query"]["$ref"].rsplit("/", 1)[-1]].get("required")
                    else " | None = None"
                )
            ]
            tsargs += [
                "query: " + tst(op["query"]) + " = {}"
                if not schemas[op["query"]["$ref"].rsplit("/", 1)[-1]].get("required")
                else "query: " + tst(op["query"])
            ]
        pyargs += ["*, options: RequestOptions | None = None"]
        tsargs += ["options: RequestOptions = {}"]
        path = op["path"]
        tspath = "`" + re.sub(r"\{(\w+)\}", r"${this.segment(\1)}", path) + "`"
        pypath = "f" + repr(re.sub(r"\{(\w+)\}", r"{self.segment(\1)}", path))
        tr = (
            "Uint8Array"
            if op.get("binaryResponse")
            else "void"
            if op["response"] == {"type": "null"}
            else tst(op["response"])
        )
        pr = "bytes" if op.get("binaryResponse") else pyt(op["response"])
        hasbody = "body" in op or op.get("binaryBody")
        hasquery = "query" in op
        tswire = "".join(
            f"{key}: {json.dumps(op[key])}, "
            for key in ("contentType", "textResponse", "redirectResponse")
            if key in op
        )
        pywire = "".join(
            f"{key}={op[wire]!r}, "
            for key, wire in [
                ("content_type", "contentType"),
                ("text_response", "textResponse"),
                ("redirect_response", "redirectResponse"),
            ]
            if wire in op
        )
        tsclient += [
            "  /** " + op["method"] + " " + path + " */",
            f"  {tname}({', '.join(tsargs)}): Promise<{tr}> {{",
            f"    return this.request({json.dumps(op['method'])}, {tspath}, "
            + ("{ body, " if hasbody else "{ ")
            + ("query, " if hasquery else "")
            + tswire
            + f"binaryBody: {str(bool(op.get('binaryBody'))).lower()}, binaryResponse: {str(bool(op.get('binaryResponse'))).lower()}, ...options"
            + " });",
            "  }",
        ]
        for asynchronous in (False, True):
            prefix = "async " if asynchronous else ""
            await_ = "await " if asynchronous else ""
            pyclients[asynchronous] += [
                f"    {prefix}def {name}(self, {', '.join(pyargs)}) -> {pr}:",
                f'        """{op["method"]} {path}."""',
                f"        return cast({json.dumps(pr)}, {await_}self.request({op['method']!r}, {pypath}, "
                + ("body=body, " if hasbody else "")
                + ("query=query, " if hasquery else "")
                + pywire
                + f"binary_body={bool(op.get('binaryBody'))}, binary_response={bool(op.get('binaryResponse'))}, options=options))",
                "",
            ]
    tsclient += ["}"]
    outputs["typescript/src/api.ts"] = "\n".join(tsclient) + "\n"
    pyclients[False][1] = "from typing import cast, Union"
    outputs["python/src/haarness_sdk/api.py"] = (
        "\n".join(pyclients[False] + pyclients[True]).rstrip() + "\n"
    )
    outputs["OPERATIONS.md"] = "\n".join(docs) + "\n"
    outputs["typescript/OPERATIONS.md"] = outputs["OPERATIONS.md"]
    return outputs


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    dirty = []
    from cloud_generate import outputs as cloud_outputs, SOURCE, SNAPSHOT

    if SOURCE.exists() and SOURCE.read_text() != SNAPSHOT.read_text():
        raise SystemExit(
            "Cloud OpenAPI snapshot drift: copy haarness-cloud/docs/openapi.json to sdks/cloud-openapi.json after regenerating it"
        )
    for name, content in {**generate(), **cloud_outputs()}.items():
        path = BASE / name
        if not path.exists() or path.read_text() != content:
            dirty.append(name)
            if not args.check:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
    if args.check and dirty:
        raise SystemExit("Generated SDK drift: " + ", ".join(dirty))
    print("SDK generation " + ("verified" if args.check else "complete"))
