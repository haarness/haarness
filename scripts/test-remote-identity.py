#!/usr/bin/env python3
"""Connector identities in a configuration folder two computers have, against a fake Cloud.

  scripts/test-remote-identity.py [state-folder]   (HAARNESS_BIN picks the binary, default target/debug/haarness)

Migration Assistant copies the configuration folder `remote enable` keeps its
identities in, and a synced or shared dotfiles folder keeps two computers'
in step. Each computer here is a folder of its own (home, data, Cloud
sign-in, project with the echo provider), and `HAARNESS_TEST_MACHINE_ID`,
honoured only with `HAARNESS_TEST_PROVIDER`, stands in for its hardware id.
The fake Cloud registers hosts (keeping a known host's name, as Cloud does),
lists them and holds one relay connection per host, closing the older one
when a host connects again, as Cloud does.

1. Studio shares itself: its identity file is named for Studio and stamped
   as Studio's.
2. Studio's configuration folder is copied to Laptop. There, before Laptop
   connects, the account bridge (Desktop's Your computers) lists Studio,
   `remote status` lists nothing and `remote forget` of Studio's host is
   refused, leaving it registered.
3. Laptop shares itself: a host of its own under its own name, and both stay
   connected for 15 seconds without taking the connection from each other.
   Each bridge lists the other computer only; restarting either keeps its id.
4. Mini and Air share one configuration folder: started and each restarted
   twice, they keep two ids, and each `remote status` lists only its own.
5. An identity from before names carried the computer is taken over by the
   computer whose name it has (same id, stamped, renamed), and a copy of it
   on a computer of another name is left alone while that one starts afresh.
6. A computer that cannot tell its hardware id (an empty override) uses the
   name every computer used before and keeps the identity there as it is.
7. Without `HAARNESS_TEST_PROVIDER` the override is ignored: Desk, given
   Studio's stand-in id and Studio's folder, still reads its own hardware id.

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
    def __init__(self, root: pathlib.Path, cloud: Cloud, name: str, machine, test_provider: bool = True, config=None) -> None:
        self.name = name
        self.folder = root / name
        for sub in ("home", "data", "account", "project/.haarness"):
            (self.folder / sub).mkdir(parents=True)
        # Where this data folder keeps its configuration, and so identities.
        self.config = self.folder / "data/config"
        if config is not None:
            self.config.symlink_to(config, target_is_directory=True)
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
        self.cloud = cloud

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

    def identity_file(self) -> pathlib.Path:
        """The file this computer's connector keeps its identity in."""
        machine = self.env.get("HAARNESS_TEST_MACHINE_ID") or None
        return self.config / file_name(self.cloud, machine)

    def identity(self) -> dict:
        path = self.identity_file()
        return json.loads(path.read_text()) if path.exists() else {}

    def status_ids(self) -> list[str]:
        return [item["id"] for item in json.loads(self.cli("remote", "status", "--json"))["items"]]

    def forget(self, id: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [BIN, "--workspace", str(self.project), "remote", "forget", id], env=self.env, capture_output=True, text=True, timeout=60
        )

    def others(self) -> list[str]:
        """What the account bridge lists for Desktop on this computer."""
        listed = json.loads(self.cli("account", "--rpc", input=json.dumps({"action": "remote-hosts"})))
        return sorted(item["name"] for item in listed["items"])

    def copied_lines(self) -> list[str]:
        return [line for line in self.log.read_text().splitlines() if line.startswith(COPIED)] if self.log.exists() else []


def stamp(machine: str, host: str) -> str:
    return hashlib.sha256(f"{machine}|{host}".encode()).hexdigest()


def file_name(cloud: Cloud, machine: str | None) -> str:
    """A host connector's identity file: named for `machine`, or the name
    every computer used before names carried the computer."""
    key = f"{cloud.url}/|{ACCOUNT}||None" + (f"|{machine}" if machine else "")
    return f"remote-{hashlib.sha256(key.encode()).hexdigest()}.json"


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
        check(studio.identity().get("machine") == stamp("MACHINE-STUDIO", first), "Studio's identity is named for Studio and stamped as Studio's")

        # 2. Its configuration folder is copied to Laptop.
        laptop.config.mkdir(parents=True, exist_ok=True)
        for path in studio.config.glob("remote-*"):
            shutil.copy2(path, laptop.config / path.name)
        check(laptop.others() == ["Studio"], "before Laptop connects, its bridge lists Studio", laptop.others())
        check(laptop.status_ids() == [], "Laptop's remote status does not list Studio's connector", laptop.status_ids())
        forgot = laptop.forget(first)
        check(forgot.returncode != 0 and first in cloud.hosts and first in cloud.connected(), "remote forget on Laptop leaves Studio's host alone", forgot.stderr)

        # 3. Laptop shares itself: a host of its own, and no fight.
        laptop.start()
        second = connected_as(laptop, cloud)
        check(second != first, "Laptop connects as a new host", (first, second))
        check(not laptop.copied_lines(), "without ever reading the copy as its own")
        check(laptop.identity().get("machine") == stamp("MACHINE-LAPTOP", second), "Laptop's identity is its own file, stamped as Laptop's")
        check(
            json.loads((laptop.config / file_name(cloud, "MACHINE-STUDIO")).read_text())["id"] == first,
            "the copy of Studio's identity is left as it was",
        )
        check(
            cloud.hosts[first]["name"] == "Studio" and cloud.hosts[second]["name"] == "Laptop",
            "each host has its own computer's name",
            {id: host["name"] for id, host in cloud.hosts.items()},
        )
        settled = time.time()
        time.sleep(15)
        opens = [id for at, id in cloud.opens if at >= settled]
        takeovers = [id for at, id in cloud.takeovers if at >= settled - 5]
        check(cloud.connected() >= {first, second}, "both stay connected", sorted(cloud.connected()))
        check(not opens and not takeovers, "neither takes the connection from the other", {"opens": opens, "takeovers": takeovers})
        check(studio.others() == ["Laptop"] and laptop.others() == ["Studio"], "each bridge lists the other computer only", (studio.others(), laptop.others()))
        check(laptop.status_ids() == [second], "Laptop's remote status lists its own connector only", laptop.status_ids())
        for computer, id in ((studio, first), (laptop, second)):
            computer.stop()
            computer.start()
            check(connected_as(computer, cloud) == id, f"{computer.name} keeps host …{id[-8:]} when it starts again")
        studio.stop()
        laptop.stop()

        # 4. One configuration folder for two computers.
        shared = root / "shared-config"
        shared.mkdir()
        mini = Computer(root, cloud, "Mini", "MACHINE-MINI", config=shared)
        air = Computer(root, cloud, "Air", "MACHINE-AIR", config=shared)
        computers += [mini, air]
        before = set(cloud.hosts)
        ids = {}
        for computer in (mini, air):
            computer.start()
            ids[computer.name] = {connected_as(computer, cloud)}
        for _ in range(2):
            for computer in (mini, air):
                computer.stop()
                computer.start()
                ids[computer.name].add(connected_as(computer, cloud))
        check(all(len(found) == 1 for found in ids.values()) and len(set().union(*ids.values())) == 2, "in one shared folder each computer keeps one id over three starts", ids)
        check(len(set(cloud.hosts) - before) == 2, "and Cloud has two hosts for them, no more", sorted(set(cloud.hosts) - before))
        check(
            mini.status_ids() == list(ids["Mini"]) and air.status_ids() == list(ids["Air"]),
            "each one's remote status lists its own connector only",
            (mini.status_ids(), air.status_ids()),
        )
        mini.stop()
        air.stop()

        # 5. An identity from before names carried the computer.
        old = studio.identity()
        old.pop("machine")
        legacy = studio.config / file_name(cloud, None)
        studio.identity_file().unlink()
        legacy.write_text(json.dumps(old))
        (laptop.config / file_name(cloud, None)).write_text(json.dumps(old))
        (laptop.config / file_name(cloud, "MACHINE-LAPTOP")).unlink()
        studio.start()
        check(connected_as(studio, cloud) == first, "an identity from before keeps its host on the computer of its name")
        check(not legacy.exists() and studio.identity().get("machine") == stamp("MACHINE-STUDIO", first), "and moves to that computer's own file, stamped")
        laptop.start()
        renewed = connected_as(laptop, cloud)
        check(renewed not in (first, second), "a copy of it on a computer of another name starts afresh", renewed)
        check((laptop.config / file_name(cloud, None)).exists(), "and is left where it was")
        check(laptop.status_ids() == [renewed], "and no longer counts as Laptop's", laptop.status_ids())
        check(len(cloud.connected() & {first, renewed}) == 2, "both connected")
        studio.stop()
        laptop.stop()

        # 6. A computer that cannot tell its hardware id changes nothing.
        attic = Computer(root, cloud, "Attic", "")
        computers.append(attic)
        attic.config.mkdir(parents=True, exist_ok=True)
        (attic.config / file_name(cloud, None)).write_text(json.dumps(old))
        attic.start()
        check(connected_as(attic, cloud) == first, "without a hardware id the identity under the old name keeps its host")
        check("machine" not in attic.identity(), "and gets no stamp")
        attic.stop()

        # 7. The stand-in id needs the test providers.
        desk = Computer(root, cloud, "Desk", "MACHINE-STUDIO", test_provider=False)
        computers.append(desk)
        desk.config.mkdir(parents=True, exist_ok=True)
        for path in studio.config.glob("remote-*"):
            shutil.copy2(path, desk.config / path.name)
        desk.start()
        # Its file is named for its real hardware id: find it by its pid.
        def desk_identity():
            for path in desk.config.glob("remote-*.json"):
                identity = json.loads(path.read_text())
                if identity.get("pid") == desk.connector.pid and identity.get("registered") and identity["id"] in cloud.connected():
                    return identity
            return None
        third = wait_for(desk_identity, 90, "Desk to connect")
        check(third["id"] != first and third.get("machine") != stamp("MACHINE-STUDIO", third["id"]), "without the test providers the override is ignored", third["id"])
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
