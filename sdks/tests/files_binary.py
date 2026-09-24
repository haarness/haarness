"""Live REST contract for binary uploads and revision-safe writes (isolated native server)."""
import base64
import hashlib
import json
import urllib.error
import urllib.request
from server import Fixture

with Fixture() as server:
    def request(path, body, status=200):
        req = urllib.request.Request(server.url+path, data=json.dumps(body).encode(), method="PUT", headers={"Authorization": "Bearer "+server.token, "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=10) as response:
                assert response.status == status
                return json.load(response)
        except urllib.error.HTTPError as error:
            assert error.code == status, (error.code, error.read().decode())
    data = bytes(range(256))*4096
    first = request("/v1/files/content", {"path":"binary.bin", "content":base64.b64encode(data).decode(), "encoding":"base64", "revision":hashlib.sha256(b"").hexdigest()})
    loaded = server.json("/v1/files/content?path=binary.bin")
    assert loaded["encoding"] == "base64"
    assert base64.b64decode(loaded["content"]) == data
    assert loaded["revision"] == hashlib.sha256(data).hexdigest()
    request("/v1/files/content", {"path":"binary.bin","content":"oops","revision":hashlib.sha256(b"").hexdigest()}, 409)
    assert (server.workspace/"binary.bin").read_bytes() == data
    request("/v1/files/content", {"path":"../escape.bin","content":"bad","revision":first["revision"]}, 400)
    request("/v1/files/content", {"path":"invalid.bin","content":"not base64!","encoding":"base64","revision":hashlib.sha256(b"").hexdigest()}, 400)
    assert not (server.workspace/"invalid.bin").exists()
    print("Binary REST round-trip, conflict preservation and invalid payload/path rejection passed")
