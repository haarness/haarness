"""Complete Cloud SDK generation from the versioned Cloud OpenAPI document.

No route allowlist: every documented operation must have a method or a stream.
The sibling source is compared when present; standalone SDK consumers use the snapshot.
"""

import json
import os
import re
from pathlib import Path
from generate import BASE, generate, obj, ref

SOURCE = Path(os.environ.get("HAARNESS_CLOUD_REPO", str(BASE.parents[1] / "haarness-cloud"))) / "docs/openapi.json"
SNAPSHOT = BASE / "cloud-openapi.json"


def specification():
    doc = json.loads(SNAPSHOT.read_text())
    schemas = doc["components"]["schemas"]
    schemas["HttpRedirect"] = obj(
        {"status": {"type": "integer"}, "location": {"type": ["string", "null"]}},
        ["status", "location"],
    )
    ops = []
    for path, methods in doc["paths"].items():
        for method, spec in methods.items():
            name = spec["operationId"]
            op = {
                "name": name,
                "method": method.upper(),
                "path": path,
                "pathParams": [],
                "response": {"type": "null"},
            }
            query = {}
            required = []
            for p in spec.get("parameters", []):
                if p["in"] == "path":
                    op["pathParams"].append(p["name"])
                if p["in"] == "query":
                    query[p["name"]] = p["schema"]
                    if p.get("required"):
                        required.append(p["name"])
            if query:
                qname = "".join(x.title() for x in name.split("_")) + "Query"
                schemas[qname] = obj(query, required)
                op["query"] = ref(qname)
            contents = spec.get("requestBody", {}).get("content", {})
            if contents:
                content, wire = next(iter(contents.items()))
                op["contentType"] = content
                if content == "application/zip":
                    op["binaryBody"] = True
                else:
                    schema = wire.get("schema", {})
                    if (
                        content == "application/x-www-form-urlencoded"
                        and schema.get("type") == "string"
                    ):
                        # OAuth declares a form-encoded string. Accept named form fields instead.
                        schema = {
                            "type": "object",
                            "additionalProperties": {"type": "string"},
                        }
                    if "$ref" not in schema:
                        bname = "".join(x.title() for x in name.split("_")) + "Body"
                        schemas[bname] = schema
                        schema = ref(bname)
                    op["body"] = schema
            success = [
                (code, r)
                for code, r in spec["responses"].items()
                if code.startswith(("2", "3"))
            ]
            if any(code.startswith("3") for code, _ in success):
                op["redirectResponse"] = True
            for code, response in success:
                if code.startswith("3"):
                    continue
                contents = response.get("content", {})
                if contents:
                    content, wire = next(iter(contents.items()))
                    op["response"] = wire.get("schema", {})
                    if content == "application/zip":
                        op["binaryResponse"] = True
                    if content == "text/html":
                        op["textResponse"] = True
                        op["response"] = {"type": "string"}
                elif code == "200":
                    # A few login acknowledgements are JSON without a declared response DTO.
                    op["response"] = {}
            if op.get("redirectResponse"):
                current = op["response"]
                op["response"] = (
                    ref("HttpRedirect")
                    if current == {"type": "null"}
                    else {"anyOf": [current, ref("HttpRedirect")]}
                )
            if name == "entity_events":
                op["stream"] = "websocket"
                op["response"] = ref("CloudEntityEvent")
            ops.append(op)
    names = [o["name"] for o in ops]
    assert len(names) == len(set(names)), "Duplicate Cloud operation ID"
    for name in re.findall(
        r'"\$ref": "#/components/schemas/([^" ]+)"', json.dumps([ops, schemas])
    ):
        assert name in schemas, "Missing Cloud schema " + name
    return doc, ops


def outputs():
    doc, ops = specification()
    generated = generate(doc, ops)
    result = {}
    for name, content in generated.items():
        if name == "openapi.json":
            continue
        if name == "operations.json":
            name = "cloud-operations.json"
        elif name.endswith("OPERATIONS.md"):
            name = name.replace("OPERATIONS.md", "CLOUD-OPERATIONS.md")
        elif name.endswith("/types.ts"):
            name = name.replace("/types.ts", "/cloud-types.ts")
        elif name.endswith("/types.py"):
            name = name.replace("/types.py", "/cloud_types.py")
        elif name.endswith("/api.ts"):
            name = name.replace("/api.ts", "/cloud-api.ts")
            content = (
                content.replace('"./transport.js"', '"./cloud-transport.js"')
                .replace('"./types.js"', '"./cloud-types.js"')
                .replace("export class Api ", "export class CloudApi ")
            )
            # These existing mutations originally accepted options in the query slot.
            # Preserve that call shape while adding the account query, including body+options.
            for method, query_type in [("billingPortal", "BillingPortalQuery"), ("cancelSubscription", "CancelSubscriptionQuery"), ("resumeSubscription", "ResumeSubscriptionQuery"), ("patchWallet", "PatchWalletQuery")]:
                pattern = rf"(  {method}\([^\n]*?)query: T\.{query_type} = \{{\}}, options: RequestOptions = \{{\}}(\): Promise<[^\n]+\{{\n)"
                replacement = rf"\1query: T.{query_type} | RequestOptions = {{}}, options: RequestOptions = {{}}\2    if ('signal' in query || 'headers' in query || 'timeoutMs' in query) {{ options = {{ ...query, ...options }}; query = {{}}; }}\n"
                content = re.sub(pattern, replacement, content)
        elif name.endswith("/api.py"):
            name = name.replace("/api.py", "/cloud_api.py")
            content = (
                content.replace(
                    "from . import types as T", "from . import cloud_types as T"
                )
                .replace("from .transport import", "from .cloud_transport import")
                .replace("class Api(", "class CloudApi(")
                .replace("class AsyncApi(", "class AsyncCloudApi(")
            )
        result[name] = content
    return result
