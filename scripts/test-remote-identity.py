#!/usr/bin/env python3
"""A connector identity copied to another computer, against a fake Cloud.

  scripts/test-remote-identity.py [state-folder]   (HAARNESS_BIN picks the binary, default target/debug/haarness)

Migration Assistant, or a synced configuration folder, copies the identity
`remote enable` keeps beside the configuration. Each computer here is a
folder of its own (home, data, Cloud sign-in, project with the echo
provider), and `HAARNESS_TEST_MACHINE_ID`, honoured only with
`HAARNESS_TEST_PROVIDER`, stands in for its hardware id. The fake Cloud
registers hosts (keeping a known host's name, as Cloud does), lists them and
holds one relay connection per host, closing the older one when a host
connects again, as Cloud does.

1. Studio shares itself; its identity is stamped as Studio's.
2. Studio's identity is copied to Laptop. Before Laptop connects, Laptop's
   account bridge (what Desktop lists under Your computers) lists Studio.
3. Laptop shares itself: it registers a new host under its own name, says so
   once, and both stay connected for 15 seconds without taking the connection
   from each other. Each bridge lists the other computer and not itself.
4. Restarting either keeps its host id.
5. An identity written before identities were stamped is adopted: Studio's,
   with its stamp removed, keeps its id and is stamped again.
6. A computer that cannot tell its hardware id (an empty override) leaves a
   copied identity as it is.
7. Without `HAARNESS_TEST_PROVIDER` the override is ignored: Desk, given
   Studio's stand-in id, still reads its own and moves off Studio's host.

Writes secret-free results to checks.json in a new folder under the state
folder (default: a temporary folder, removed when the run passes).
"""

import asyncio
import datetime
import hashlib
import json
import os
import pathlib
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

import e2e_scratch

REPO = pathlib.Path(__file__).resolve().parents[1]
BIN = str(pathlib.Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve())
ACCOUNT = "00000000-0000-4000-8000-00000000c001"
CONFIG = '[defaults]\nprovider="echo"\nmodel="echo-1"\n[lsp]\nenabled=false\n'
COPIED = "This identity was copied from another computer"


class Cloud:
    """The fake Cloud, on its own thread: what it registered, and who holds
    each host's relay connection."""

    def __init__(self) -> None:
        self.hosts: dict[str, dict] = {}
        self.sockets: dict[str, object] = {}
        self.opens: list[tuple[float, str]] = []
        self.takeovers: list[tuple[float, str]] = []
        self.registrations: list[tuple[float, str, str]] = []
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            self.port = probe.getsockname()[1]
        self.url = f"http://127.0.0.1:{self.port}"
        self.ready = threading.Event()
        self.loop = asyncio.new_event_loop()
        threading.Thread(target=self.run, daemon=True).start()
        if not self.ready.wait(20):
            raise SystemExit("FAIL: the fake Cloud did not start")

    def run(self) -> None:
        from aiohttp import web

        async def me(request):
            return web.json_response(
                {
                    "user": {"id": "u1", "displayName": "Sam Rivera", "email": "sam@example.com"},
                    "accounts": [{"id": ACCOUNT, "name": "Sam Rivera", "role": "owner"}],
                    "defaultAccountId": ACCOUNT,
                }
            )

        async def token(request):
            return web.json_response({"access_token": "renewed", "refresh_token": "next", "expires_in": 3600})

        async def register(request):
            body = await request.json()
            known = self.hosts.get(body["id"])
            self.registrations.append((time.time(), body["id"], body.get("name", "")))
            self.hosts[body["id"]] = {
                "id": body["id"],
                "accountId": ACCOUNT,
                "name": known["name"] if known else body.get("name"),
                "mode": body.get("mode"),
                "metadata": body.get("metadata", {}),
            }
            return web.json_response(self.row(body["id"]))

        async def listing(request):
            return web.json_response({"items": [self.row(id) for id in self.hosts], "nextCursor": None})

        async def host(request):
            id = request.match_info["id"]
            if id not in self.hosts:
                return web.json_response({"error": {"message": "Not found"}}, status=404)
            return web.json_response(self.row(id))

        async def connect(request):
            id = request.match_info["id"]
            if id not in self.hosts:
                return web.json_response({"error": {"message": "Not found"}}, status=404)
            ws = web.WebSocketResponse(autoping=True)
            await ws.prepare(request)
            self.opens.append((time.time(), id))
            older = self.sockets.get(id)
            self.sockets[id] = ws
            if older is not None:
                # One computer per host: the newer connection takes it.
                self.takeovers.append((time.time(), id))
                await older.close(code=4000, message=b"Connected elsewhere")
            await ws.send_str(json.dumps({"type": "ready", "version": 1}))
            async for _ in ws:
                pass
            if self.sockets.get(id) is ws:
                del self.sockets[id]
            return ws

        app = web.Application()
        app.router.add_get("/api/v1/me", me)
        app.router.add_post("/oauth/token", token)
        app.router.add_post("/api/v1/remote-hosts", register)
        app.router.add_get("/api/v1/remote-hosts", listing)
        app.router.add_get("/api/v1/remote-hosts/{id}", host)
        app.router.add_get("/api/v1/remote-hosts/{id}/connect", connect)

        async def start():
            runner = web.AppRunner(app)
            await runner.setup()
            await web.TCPSite(runner, "127.0.0.1", self.port).start()
            self.ready.set()

        asyncio.set_event_loop(self.loop)
        self.loop.run_until_complete(start())
        self.loop.run_forever()

    def row(self, id: str) -> dict:
        return {**self.hosts[id], "connected": id in self.sockets, "lastSeenAt": None}

    def connected(self) -> set[str]:
        return set(self.sockets)


class Computer:
    def __init__(self, root: pathlib.Path, cloud: Cloud, name: str, machine, test_provider: bool = True) -> None:
        self.name = name
        self.folder = root / name
        for sub in ("home", "data", "account", "project/.haarness"):
            (self.folder / sub).mkdir(parents=True)
        (self.folder / "project/.haarness/config.toml").write_text(CONFIG if test_provider else "[lsp]\nenabled=false\n")
        self.project = self.folder / "project"
        self.env = {
            **os.environ,
            "HAARNESS_HOME": str(self.folder / "home"),
            "HAARNESS_DATA_DIR": str(self.folder / "data"),
            "HAARNESS_CLOUD_CONFIG_DIR": str(self.folder / "account"),
            "HAARNESS_CLOUD_API_URL": cloud.url,
            "HAARNESS_CLOUD_WEB_ORIGIN": cloud.url,
            "SHELL": "/bin/sh",
            "RUST_LOG": "warn",
        }
        for key in ("HAARNESS_API_KEY", "HAARNESS_TEST_PROVIDER", "HAARNESS_TEST_MACHINE_ID"):
            self.env.pop(key, None)
        if test_provider:
            self.env["HAARNESS_TEST_PROVIDER"] = "1"
        if machine is not None:
            self.env["HAARNESS_TEST_MACHINE_ID"] = machine
        expires = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)
        credentials = self.folder / "account/credentials.json"
        credentials.write_text(
            json.dumps(
                {
                    "apiUrl": cloud.url + "/",
                    "accessToken": f"access-{name}",
                    "accessExpiresAt": expires.isoformat().replace("+00:00", "Z"),
                    "refreshToken": f"refresh-{name}",
                }
            )
        )
        os.chmod(credentials, 0o600)
        self.log = self.folder / "connector.log"
        self.connector = None

    def cli(self, *args, input=None) -> str:
        result = subprocess.run(
            [BIN, "--workspace", str(self.project), *args], env=self.env, input=input, capture_output=True, text=True, timeout=120
        )
        if result.returncode != 0:
            raise SystemExit(f"FAIL: {self.name}: {' '.join(args)}: {result.stderr}")
        return result.stdout

    def start(self) -> None:
        self.connector = subprocess.Popen(
            [BIN, "--workspace", str(self.project), "remote", "run", "--name", self.name],
            stdout=open(self.log, "a"),
            stderr=subprocess.STDOUT,
            env=self.env,
            start_new_session=True,
        )

    def stop(self) -> None:
        if self.connector is not None and self.connector.poll() is None:
            os.killpg(self.connector.pid, signal.SIGTERM)
            try:
                self.connector.wait(timeout=20)
            except subprocess.TimeoutExpired:
                os.killpg(self.connector.pid, signal.SIGKILL)
                self.connector.wait()
        self.connector = None

    def identity_file(self) -> pathlib.Path | None:
        found = sorted(path for path in self.folder.rglob("remote-*.json"))
        return found[0] if found else None

    def identity(self) -> dict:
        path = self.identity_file()
        return json.loads(path.read_text()) if path else {}

    def others(self) -> list[str]:
        """What the account bridge lists for Desktop on this computer."""
        listed = json.loads(self.cli("account", "--rpc", input=json.dumps({"action": "remote-hosts"})))
        return sorted(item["name"] for item in listed["items"])

    def copied_lines(self) -> list[str]:
        return [line for line in self.log.read_text().splitlines() if line.startswith(COPIED)] if self.log.exists() else []


def stamp(machine: str, host: str) -> str:
    return hashlib.sha256(f"{machine}|{host}".encode()).hexdigest()


def wait_for(check, timeout: float, what: str):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.25)
    raise SystemExit(f"FAIL: timed out waiting for {what}")


def connected_as(computer: Computer, cloud: Cloud) -> str:
    """The host id `computer` connects as, once its running connector (not a
    copied file) registered and Cloud holds its connection."""

    def check():
        identity = computer.identity()
        id = identity.get("id")
        ours = identity.get("pid") == computer.connector.pid
        return id if ours and identity.get("registered") and id in cloud.connected() else None

    return wait_for(check, 90, f"{computer.name} to connect")


def main() -> None:
    state = pathlib.Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else None
    if state is not None:
        state.mkdir(parents=True, exist_ok=True)
    root = pathlib.Path(tempfile.mkdtemp(prefix="remote-identity-", dir=state))
    cloud = Cloud()
    checks: list[str] = []
    computers: list[Computer] = []

    def check(passed: bool, line: str, detail=None) -> None:
        if not passed:
            raise SystemExit(f"FAIL: {line}: {detail!r}")
        checks.append(f"PASS {line}")
        print(f"PASS {line}", flush=True)

    try:
        studio = Computer(root, cloud, "Studio", "MACHINE-STUDIO")
        laptop = Computer(root, cloud, "Laptop", "MACHINE-LAPTOP")
        computers += [studio, laptop]

        # 1. Studio shares itself.
        studio.start()
        first = connected_as(studio, cloud)
        check(studio.identity().get("machine") == stamp("MACHINE-STUDIO", first), "Studio's identity carries Studio's stamp")

        # 2. Its identity is copied to Laptop, at the same place in its folder.
        source = studio.identity_file()
        target = laptop.folder / source.relative_to(studio.folder)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        check(laptop.others() == ["Studio"], "before Laptop connects, its bridge lists Studio", laptop.others())

        # 3. Laptop shares itself: a host of its own, and no fight.
        laptop.start()
        second = connected_as(laptop, cloud)
        check(second != first, "Laptop connects as a new host", (first, second))
        check(len(laptop.copied_lines()) == 1, "Laptop says once that its identity was copied", laptop.copied_lines())
        check(first in laptop.copied_lines()[0] and second in laptop.copied_lines()[0], "the line names both hosts", laptop.copied_lines())
        check(laptop.identity().get("machine") == stamp("MACHINE-LAPTOP", second), "Laptop's identity carries Laptop's stamp")
        check(
            cloud.hosts[first]["name"] == "Studio" and cloud.hosts[second]["name"] == "Laptop",
            "each host has its own computer's name",
            {id: host["name"] for id, host in cloud.hosts.items()},
        )
        settled = time.time()
        time.sleep(15)
        opens = [id for at, id in cloud.opens if at >= settled]
        takeovers = [id for at, id in cloud.takeovers if at >= settled - 5]
        check(cloud.connected() == {first, second}, "both stay connected", sorted(cloud.connected()))
        check(not opens and not takeovers, "neither takes the connection from the other", {"opens": opens, "takeovers": takeovers})
        check(not studio.copied_lines(), "Studio keeps its identity as it was")
        check(studio.others() == ["Laptop"] and laptop.others() == ["Studio"], "each bridge lists the other computer only", (studio.others(), laptop.others()))

        # 4. Restarting either keeps its host.
        for computer, id in ((studio, first), (laptop, second)):
            computer.stop()
            computer.start()
            check(connected_as(computer, cloud) == id, f"{computer.name} keeps host …{id[-8:]} when it starts again")
        check(len(laptop.copied_lines()) == 1, "Laptop does not move again")

        # 5. An identity from before identities were stamped is adopted.
        studio.stop()
        path = studio.identity_file()
        old = json.loads(path.read_text())
        old.pop("machine")
        path.write_text(json.dumps(old))
        studio.start()
        check(connected_as(studio, cloud) == first, "an unstamped identity keeps its host")
        check(studio.identity().get("machine") == stamp("MACHINE-STUDIO", first), "and is stamped as this computer's")
        check(not studio.copied_lines(), "without saying it was copied")
        studio.stop()
        laptop.stop()

        # 6. A computer that cannot tell its hardware id changes nothing.
        attic = Computer(root, cloud, "Attic", "")
        computers.append(attic)
        target = attic.folder / source.relative_to(studio.folder)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)
        before = attic.identity()
        attic.start()
        check(connected_as(attic, cloud) == first, "without a hardware id a copied identity keeps its host")
        check(attic.identity().get("machine") == before.get("machine"), "and keeps the stamp it came with")
        check(not attic.copied_lines(), "and says nothing about a copy")
        attic.stop()

        # 7. The stand-in id needs the test providers.
        desk = Computer(root, cloud, "Desk", "MACHINE-STUDIO", test_provider=False)
        computers.append(desk)
        target = desk.folder / source.relative_to(studio.folder)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)
        desk.start()
        third = connected_as(desk, cloud)
        check(third not in (first, second) and len(desk.copied_lines()) == 1, "without the test providers the override is ignored", third)
        desk.stop()
        (root / "checks.json").write_text(json.dumps({"verdict": "PASS", "checks": checks}, indent=2))
    except BaseException as error:
        (root / "checks.json").write_text(json.dumps({"verdict": "FAIL", "error": str(error), "checks": checks}, indent=2))
        for computer in computers:
            if computer.log.exists():
                print(f"--- {computer.name} connector output\n{computer.log.read_text()[-3000:]}", flush=True)
        raise
    finally:
        for computer in computers:
            computer.stop()
        # A project server a stopped connector was still starting comes up
        # after the first stop, so each is stopped once more a moment later.
        for attempt in range(2):
            if attempt:
                time.sleep(3)
            for computer in computers:
                subprocess.run(
                    [BIN, "--workspace", str(computer.project), "serve", "--stop"], env=computer.env, capture_output=True, timeout=60
                )
        cloud.loop.call_soon_threadsafe(cloud.loop.stop)
        # A state folder named on the command line is the caller's to keep.
        if state is None:
            e2e_scratch.settle(root)
        else:
            print(f"Results in {root}", flush=True)


if __name__ == "__main__":
    main()
