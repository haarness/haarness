#!/usr/bin/env python3
"""Connector identities of computers that share a configuration folder, or run several releases, against a fake Cloud.

  scripts/test-remote-identity.py [state-folder]
  HAARNESS_BIN picks the binary (default target/debug/haarness); HAARNESS_BASE_BIN,
  the release before it, adds the upgrade, downgrade and mixed-release cases, and
  HAARNESS_IDENTITY_RELEASES_ONLY=1 runs only those.

Migration Assistant copies the configuration folder `remote enable` keeps its
identities in, a synced or shared dotfiles folder keeps two computers' in
step, and Desktop and the terminal update separately while sharing it. Each
computer here is a folder of its own (home, data, Cloud sign-in, project with
the echo provider), and `HAARNESS_TEST_MACHINE_ID`, honoured only with
`HAARNESS_TEST_PROVIDER`, stands in for its hardware id. The fake Cloud
registers hosts (keeping a known host's name, as Cloud does), lists them and
holds one relay connection per host, closing the older one when a host
connects again, as Cloud does.

1. Studio shares itself: its identity stays under the name every release
   looks under, stamped as Studio's.
2. Studio's folder is copied to Laptop. There, before Laptop connects, the
   account bridge (Desktop's Your computers) lists Studio, `remote status`
   lists nothing and `remote forget` of Studio's host is refused.
3. Laptop shares itself: a host of its own, in a file named for Laptop, and
   both stay connected for 15 seconds without taking the connection from each
   other. Each bridge lists the other only; restarts keep each id.
4. Mini and Air share one folder: started and each restarted twice, they keep
   two ids, and each `remote status` lists only its own.
5. A computer that cannot tell its hardware id (an empty override) keeps an
   unstamped identity under the old name as it is. A start whose id cannot be
   read never connects as another computer: Studio and Mini, whose stamped
   identity a second computer could hold too, and Laptop and Air, whose own
   file cannot be told from another computer's either, do not connect, and
   Studio stays connected meanwhile. Desktop's restore fails, so Desktop
   tries again, and once the id reads it turns access back on with the same
   host. Air, unable to read its id, does not list, stop or forget Mini's
   running connector, and signing out leaves it running.
6. Without `HAARNESS_TEST_PROVIDER` the override is ignored.

With HAARNESS_BASE_BIN:

7. Upgrade: Den shared by the release before keeps its host under the new one,
   stamped where it was. Downgrade: the release before then keeps it too.
8. Mixed releases: with the new connector running, the release before's
   `remote enable` says it already runs and no second host appears; signing
   out with the new build stops a connector the release before runs here.
9. A copy made by the release before on a computer of another name (Annex)
   is adopted with its id, as every identity without a stamp is; split as
   REMOTE-ACCESS says, Annex gets a host of its own and Den keeps its host.
10. Renamed computers, whose release before ran its connector under a name
    they no longer have: with nothing running, the new release's terminal
    start and Desktop's restore each keep the host; with it still running,
    Desktop's restore makes it this computer's, and once it has stopped the
    next restore turns access back on with the same host.

Either way, from an identity written as the release before leaves one, and
with HAARNESS_BASE_BIN once more from the folder that release itself left:

11. One folder Bay and Cove keep in step, one host under the release before:
    Bay runs this release first and keeps the host; Cove's Desktop then turns
    access back on as a host of its own, both stay connected, and Cove keeps
    that host when Desktop starts again.
12. Cove turns access off and forgets its host, then forgets one whose
    connector stopped by itself: each time Desktop starting there leaves
    access off and registers nothing, while turning it on registers a host
    afresh.
13. Cove's host is removed in Cloud while its connector runs: Desktop's
    status check reads off and deletes the identity, and Desktop starting
    there leaves access off.

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
import uuid

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

        async def revoke(request):
            return web.json_response({})

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

        async def delete(request):
            id = request.match_info["id"]
            if self.hosts.pop(id, None) is None:
                return web.json_response({"error": {"message": "Not found"}}, status=404)
            ws = self.sockets.pop(id, None)
            if ws is not None:
                await ws.close(code=4403, message=b"Removed")
            return web.json_response({})

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
        app.router.add_post("/oauth/revoke", revoke)
        app.router.add_post("/api/v1/remote-hosts", register)
        app.router.add_get("/api/v1/remote-hosts", listing)
        app.router.add_get("/api/v1/remote-hosts/{id}", host)
        app.router.add_delete("/api/v1/remote-hosts/{id}", delete)
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

    def remove(self, id: str) -> None:
        """Remove access to host `id`, as a phone or the web does, before its
        connector hears of it: its connection is still up."""
        self.hosts.pop(id, None)

    def known(self, id: str, name: str) -> None:
        """Host `id`, registered by an earlier release."""
        self.hosts[id] = {"id": id, "accountId": ACCOUNT, "name": name, "mode": "host", "metadata": {}}


class Computer:
    def __init__(self, root: pathlib.Path, cloud: Cloud, name: str, machine, test_provider: bool = True, config=None) -> None:
        self.name = name
        self.cloud = cloud
        self.folder = root / name
        for sub in ("home", "data", "account", "project/.haarness", "chat-home"):
            (self.folder / sub).mkdir(parents=True)
        (self.folder / "project/.haarness/config.toml").write_text(CONFIG if test_provider else "[lsp]\nenabled=false\n")
        self.project = self.folder / "project"
        # Where this data folder keeps its configuration, and so identities.
        self.config = self.folder / "data/config"
        if config is not None:
            self.config.symlink_to(config, target_is_directory=True)
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
        self.sign_in()
        self.log = self.folder / "connector.log"
        self.connector = None
        self.file = None

    def sign_in(self) -> None:
        expires = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)
        credentials = self.folder / "account/credentials.json"
        credentials.write_text(
            json.dumps(
                {
                    "apiUrl": self.cloud.url + "/",
                    "accessToken": f"access-{self.name}",
                    "accessExpiresAt": expires.isoformat().replace("+00:00", "Z"),
                    "refreshToken": f"refresh-{self.name}",
                }
            )
        )
        os.chmod(credentials, 0o600)

    def run(self, *args, binary=None, input=None, workspace=None) -> subprocess.CompletedProcess:
        return subprocess.run(
            [binary or BIN, "--workspace", str(workspace or self.project), *args],
            env=self.env,
            input=input,
            capture_output=True,
            text=True,
            timeout=120,
        )

    def cli(self, *args, input=None) -> str:
        result = self.run(*args, input=input)
        if result.returncode != 0:
            raise SystemExit(f"FAIL: {self.name}: {' '.join(args)}: {result.stderr}")
        return result.stdout

    def start(self, binary=None, name=None) -> None:
        self.connector = subprocess.Popen(
            [binary or BIN, "--workspace", str(self.project), "remote", "run", "--name", name or self.name],
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

    def identities(self) -> dict[pathlib.Path, dict]:
        found = {}
        for path in self.config.glob("remote-*.json") if self.config.exists() else []:
            try:
                found[path] = json.loads(path.read_text())
            except ValueError:
                pass
        return found

    def identity(self) -> dict:
        return json.loads(self.file.read_text()) if self.file and self.file.exists() else {}

    def status_ids(self) -> list[str]:
        return [item["id"] for item in json.loads(self.cli("remote", "status", "--json"))["items"]]

    def others(self) -> list[str]:
        """What the account bridge lists for Desktop on this computer."""
        listed = json.loads(self.cli("account", "--rpc", input=json.dumps({"action": "remote-hosts"})))
        return sorted(item["name"] for item in listed["items"])

    def accessing(self, change=None, check=False) -> subprocess.CompletedProcess:
        """Desktop's Remote access: the account bridge's `remote-access`,
        reporting (asking Cloud with `check`) or making `change`."""
        home = self.folder / "chat-home"
        request = {"action": "remote-access", "chatHome": str(home)}
        if change:
            request["change"] = change
        if check:
            request["check"] = True
        return self.run("--chat-home", "account", "--rpc", workspace=home, input=json.dumps(request))

    def access(self, change=None, check=False) -> dict:
        result = self.accessing(change, check)
        if result.returncode != 0:
            raise SystemExit(f"FAIL: {self.name}: remote access {change or 'status'}: {result.stderr}")
        return json.loads(result.stdout)

    def restoring(self) -> subprocess.CompletedProcess:
        """Desktop starting: the account bridge's remote access `restore`."""
        return self.accessing("restore")

    def restore(self) -> dict:
        return self.access("restore")

    def copied_lines(self) -> list[str]:
        return [line for line in self.log.read_text().splitlines() if line.startswith(COPIED)] if self.log.exists() else []


def stamp(machine: str, host: str) -> str:
    return hashlib.sha256(f"{machine}|{host}".encode()).hexdigest()


def file_name(cloud: Cloud, machine: str | None) -> str:
    """A host connector's identity file: the name every release looks under,
    or the name of a computer's own when that one is another's."""
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


def connected_as(computer: Computer, cloud: Cloud, pid=None) -> str:
    """The host id `computer`'s connector (or the one with `pid`) connects
    as, once it registered and Cloud holds its connection; remembers its file."""

    def check():
        want = pid or computer.connector.pid
        for path, identity in computer.identities().items():
            if identity.get("pid") == want and identity.get("registered") and identity.get("id") in cloud.connected():
                computer.file = path
                return identity["id"]
        return None

    return wait_for(check, 90, f"{computer.name} to connect")


def alive(pid) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except (OSError, TypeError):
        return False


def computers_apart(root, cloud, computer, check, old) -> None:
    """Steps 1 to 6: copied and shared folders, and hardware ids that fail."""
    studio = computer("Studio", "MACHINE-STUDIO")
    laptop = computer("Laptop", "MACHINE-LAPTOP")

    # 1. Studio shares itself.
    studio.start()
    first = connected_as(studio, cloud)
    check(studio.file.name == old and studio.identity().get("machine") == stamp("MACHINE-STUDIO", first), "Studio keeps its identity under the old name, stamped as Studio's", studio.file.name)

    # 2. Its configuration folder is copied to Laptop.
    laptop.config.mkdir(parents=True, exist_ok=True)
    for path in studio.config.glob("remote-*"):
        shutil.copy2(path, laptop.config / path.name)
    check(laptop.others() == ["Studio"], "before Laptop connects, its bridge lists Studio", laptop.others())
    check(laptop.status_ids() == [], "Laptop's remote status does not list Studio's connector", laptop.status_ids())
    forgot = laptop.run("remote", "forget", first)
    check(forgot.returncode != 0 and first in cloud.hosts and first in cloud.connected(), "remote forget on Laptop leaves Studio's host alone", forgot.stderr)

    # 3. Laptop shares itself: a host of its own, and no fight.
    laptop.start()
    second = connected_as(laptop, cloud)
    check(second != first and not laptop.copied_lines(), "Laptop connects as a new host, never reading the copy as its own", (first, second))
    check(laptop.file.name == file_name(cloud, "MACHINE-LAPTOP") and laptop.identity().get("machine") == stamp("MACHINE-LAPTOP", second), "in a file named for Laptop, stamped as Laptop's")
    check(json.loads((laptop.config / old).read_text())["id"] == first, "the copy of Studio's identity is left as it was")
    check(cloud.hosts[first]["name"] == "Studio" and cloud.hosts[second]["name"] == "Laptop", "each host has its own computer's name", {id: host["name"] for id, host in cloud.hosts.items()})
    settled = time.time()
    time.sleep(15)
    opens = [id for at, id in cloud.opens if at >= settled]
    takeovers = [id for at, id in cloud.takeovers if at >= settled - 5]
    check(cloud.connected() >= {first, second}, "both stay connected", sorted(cloud.connected()))
    check(not opens and not takeovers, "neither takes the connection from the other", {"opens": opens, "takeovers": takeovers})
    check(studio.others() == ["Laptop"] and laptop.others() == ["Studio"], "each bridge lists the other computer only", (studio.others(), laptop.others()))
    check(laptop.status_ids() == [second], "Laptop's remote status lists its own connector only", laptop.status_ids())
    for each, id in ((studio, first), (laptop, second)):
        each.stop()
        each.start()
        check(connected_as(each, cloud) == id, f"{each.name} keeps host …{id[-8:]} when it starts again")
    studio.stop()
    laptop.stop()

    # 4. One configuration folder for two computers.
    shared = root / "shared-config"
    shared.mkdir()
    mini = computer("Mini", "MACHINE-MINI", config=shared)
    air = computer("Air", "MACHINE-AIR", config=shared)
    before = set(cloud.hosts)
    ids = {}
    for each in (mini, air):
        each.start()
        ids[each.name] = {connected_as(each, cloud)}
    for _ in range(2):
        for each in (mini, air):
            each.stop()
            each.start()
            ids[each.name].add(connected_as(each, cloud))
    check(all(len(found) == 1 for found in ids.values()) and len(set().union(*ids.values())) == 2, "in one shared folder each computer keeps one id over three starts", ids)
    check(len(set(cloud.hosts) - before) == 2, "and Cloud has two hosts for them, no more", sorted(set(cloud.hosts) - before))
    check(mini.status_ids() == list(ids["Mini"]) and air.status_ids() == list(ids["Air"]), "each one's remote status lists its own connector only", (mini.status_ids(), air.status_ids()))
    mini.stop()
    air.stop()

    # 5. Hardware ids that cannot be read.
    attic = computer("Attic", "")
    attic.config.mkdir(parents=True, exist_ok=True)
    before_attic = {k: v for k, v in json.loads((studio.config / old).read_text()).items() if k != "machine"}
    (attic.config / old).write_text(json.dumps(before_attic))
    attic.start()
    check(connected_as(attic, cloud) == first, "without a hardware id the identity under the old name keeps its host")
    check("machine" not in attic.identity(), "and gets no stamp")
    attic.stop()

    def unread(each) -> None:
        each.env["HAARNESS_TEST_MACHINE_ID"] = ""

    def read_again(each) -> None:
        each.env["HAARNESS_TEST_MACHINE_ID"] = f"MACHINE-{each.name.upper()}"

    def refused(each, what: str) -> None:
        files = {path.name: path.read_text() for path in each.identities()}
        hosts = set(cloud.hosts)
        mark = each.log.stat().st_size if each.log.exists() else 0
        each.start()
        wait_for(lambda: each.connector.poll() is not None, 90, f"{each.name}'s connector to give up")
        said = each.log.read_text()[mark:]
        check(each.connector.returncode != 0 and "hardware ID could not be read" in said, f"{what} does not connect, and says why", said[-400:])
        check(set(cloud.hosts) == hosts and {path.name: path.read_text() for path in each.identities()} == files, "registering nothing and changing no identity")
        each.connector = None

    # Laptop, the copy of Studio's identity still beside its own: a file
    # named for a hardware id it cannot read is no more its own than
    # Studio's, so it does not connect, and Studio stays connected.
    studio.start()
    check(connected_as(studio, cloud) == first, "Studio connected")
    settled = time.time()
    unread(laptop)
    refused(laptop, "a start of Laptop that cannot read its hardware id, its own file beside the copy of Studio's")
    takeovers = [id for at, id in cloud.takeovers if at >= settled]
    check(first in cloud.connected() and second not in cloud.connected() and not takeovers, "and Studio keeps its connection", takeovers)
    read_again(laptop)
    studio.stop()
    # Studio: its stamped identity under the old name is just what a
    # computer it was copied to holds, so nothing tells them apart.
    unread(studio)
    refused(studio, "a start of Studio that cannot read its hardware id")
    failed = studio.restoring()
    check(failed.returncode != 0 and "hardware ID could not be read" in failed.stderr and first not in cloud.connected(), "Desktop's restore fails, saying why, so Desktop tries it again", failed.stdout + failed.stderr)
    read_again(studio)
    restored = studio.restore()
    pid = wait_for(lambda: studio.identity().get("pid") if alive(studio.identity().get("pid")) else None, 60, "Studio's restored connector")
    check(restored.get("state") == "on" and connected_as(studio, cloud, pid=pid) == first, "once the id reads again, the next restore turns access on with the same host", restored)
    studio.cli("remote", "disable", first)
    # The shared folder: neither Air, by its own file, nor Mini, under the
    # old name, connects.
    unread(air)
    refused(air, "in the shared folder, a start of Air that cannot read its hardware id")
    read_again(air)
    unread(mini)
    refused(mini, "a start of Mini, whose identity is the shared one, that cannot read its hardware id")
    read_again(mini)
    # Mini runs; Air, unable to read its id, leaves Mini's connector alone.
    mini_id = next(iter(ids["Mini"]))
    mini.start()
    check(connected_as(mini, cloud) == mini_id, "Mini connected")
    unread(air)
    check(air.status_ids() == [], "Air's remote status, without its hardware id, lists no stamped connector", air.status_ids())
    for command in ("disable", "forget"):
        result = air.run("remote", command, mini_id)
        check(result.returncode != 0 and "hardware ID could not be read" in result.stderr and mini.connector.poll() is None and mini_id in cloud.connected() and mini_id in cloud.hosts, f"Air's remote {command} of Mini's connector is refused, and Mini stays connected", result.stderr[-300:])
    air.cli("account", "--rpc", input=json.dumps({"action": "logout"}))
    time.sleep(2)
    check(mini.connector.poll() is None and mini_id in cloud.connected() and not (mini.config / old).with_suffix(".stop").exists(), "Air's sign-out leaves Mini's connector running, with no stop file")
    air.sign_in()
    read_again(air)
    mini.stop()

    # 6. The stand-in id needs the test providers.
    desk = computer("Desk", "MACHINE-STUDIO", test_provider=False)
    desk.config.mkdir(parents=True, exist_ok=True)
    for path in studio.config.glob("remote-*"):
        shutil.copy2(path, desk.config / path.name)
    desk.start()
    third = connected_as(desk, cloud)
    check(third != first and desk.identity().get("machine") != stamp("MACHINE-STUDIO", third), "without the test providers the override is ignored", third)
    desk.stop()


def own_connector(each: Computer, machine: str):
    """The pid of a connector running on `each` with an identity stamped as
    the computer `machine`'s."""
    for identity in each.identities().values():
        if alive(identity.get("pid")) and identity.get("machine") == stamp(machine, identity.get("id", "")):
            return identity["pid"]
    return None


def kept_in_step(root, cloud, computer, check, old, base, names) -> None:
    """Steps 11 to 13: two computers (`names`) keep one folder in step, one
    host under the release before: run by `base`, or written as it leaves
    an identity."""
    kept = root / f"{names[0].lower()}-config"
    kept.mkdir()
    machines = [f"MACHINE-{name.upper()}" for name in names]
    bay = computer(names[0], machines[0], config=kept)
    cove = computer(names[1], machines[1], config=kept)

    # 11. The first on this release keeps the host, the other's Desktop
    # turns access on as its own.
    if base:
        bay.start(base)
        bay_id = connected_as(bay, cloud)
        bay.stop()
        cove.start(base)
        check(connected_as(cove, cloud) == bay_id, "under the release before, both computers of one kept folder are one host")
        cove.stop()
    else:
        # As the release before leaves it once its connector ran: unstamped
        # under the old name, registered, its lock beside it.
        bay_id = str(uuid.uuid4())
        (kept / old).write_text(
            json.dumps({"id": bay_id, "workspace": str(bay.project), "chatId": None, "accountId": ACCOUNT, "name": bay.name, "apiUrl": cloud.url + "/", "version": "0.1.16", "registered": True})
        )
        (kept / old).with_suffix(".lock").touch()
        cloud.known(bay_id, bay.name)
        check(bay.status_ids() == [bay_id] and cove.status_ids() == [bay_id], "an identity the release before wrote in a kept folder is both computers' own until one runs this release", (bay.status_ids(), cove.status_ids()))
    bay.start()
    check(connected_as(bay, cloud) == bay_id and bay.file.name == old and bay.identity().get("machine") == stamp(machines[0], bay_id), f"{bay.name}, first on this release, keeps the host, stamped as {bay.name}'s")
    hosts = set(cloud.hosts)
    restored = cove.restore()
    pid = wait_for(lambda: own_connector(cove, machines[1]), 60, f"{cove.name}'s restored connector")
    cove_id = connected_as(cove, cloud, pid=pid)
    check(restored.get("state") == "on" and cove_id != bay_id and set(cloud.hosts) - hosts == {cove_id}, f"{cove.name}'s Desktop restore then turns access on, as a host of its own", restored)
    own = kept / file_name(cloud, machines[1])
    check(cove.file.name == own.name and json.loads((kept / old).read_text())["id"] == bay_id, f"in a file named for {cove.name}, leaving {bay.name}'s as it was", cove.file.name)
    settled = time.time()
    time.sleep(5)
    check({bay_id, cove_id} <= cloud.connected() and not [id for at, id in cloud.takeovers if at >= settled], "both stay connected, neither taking the other's connection")
    os.kill(pid, signal.SIGTERM)
    wait_for(lambda: not alive(pid), 30, f"{cove.name}'s connector to stop")
    again = cove.restore()
    pid = wait_for(lambda: own_connector(cove, machines[1]), 60, f"{cove.name}'s connector, restored again")
    check(again.get("state") == "on" and connected_as(cove, cloud, pid=pid) == cove_id and set(cloud.hosts) - hosts == {cove_id}, f"and Desktop starting again keeps {cove.name}'s host", again)

    # 12. Turned off and forgotten there, it stays off when Desktop starts.
    cove.cli("remote", "disable", cove_id)
    cove.cli("remote", "forget", cove_id)
    check(cove_id not in cloud.hosts and not own.exists(), f"{cove.name}'s remote disable and forget end its host and delete its identity")
    hosts = set(cloud.hosts)
    again = cove.restore()
    check(again.get("state") == "off" and set(cloud.hosts) == hosts and own_connector(cove, machines[1]) is None and not own.exists(), f"Desktop starting on {cove.name} then leaves access off and registers nothing", again)
    check(bay_id in cloud.connected() and json.loads((kept / old).read_text())["id"] == bay_id, f"while {bay.name} stays connected with its own host")
    on = cove.access("on")
    pid = wait_for(lambda: own_connector(cove, machines[1]), 60, f"{cove.name}'s connector turned on")
    fresh = connected_as(cove, cloud, pid=pid)
    check(on.get("state") == "on" and fresh not in hosts and set(cloud.hosts) - hosts == {fresh}, f"turning access on there registers {cove.name} afresh", on)
    # Stopped by itself (it quit, or the computer shut down), so with no
    # stop file, then forgotten.
    os.kill(pid, signal.SIGTERM)
    wait_for(lambda: not alive(pid), 30, f"{cove.name}'s connector to stop")
    cove.cli("remote", "forget", fresh)
    hosts = set(cloud.hosts)
    again = cove.restore()
    check(again.get("state") == "off" and set(cloud.hosts) == hosts and own_connector(cove, machines[1]) is None and not own.exists() and not own.with_suffix(".stop").exists(), f"forgotten after it stopped by itself, Desktop starting on {cove.name} leaves access off too", again)
    on = cove.access("on")
    pid = wait_for(lambda: own_connector(cove, machines[1]), 60, f"{cove.name}'s connector turned on again")
    fresh = connected_as(cove, cloud, pid=pid)
    check(on.get("state") == "on" and set(cloud.hosts) - hosts == {fresh}, f"and turning access on registers {cove.name} afresh again", on)

    # 13. Removed in Cloud while its connector runs.
    cloud.remove(fresh)
    status = cove.access(check=True)
    check(status.get("state") == "off" and not own.exists(), f"after Remove access, Desktop's status check on {cove.name} reads off and deletes its identity", status)
    wait_for(lambda: not alive(pid), 30, f"{cove.name}'s removed connector to stop")
    hosts = set(cloud.hosts)
    again = cove.restore()
    check(again.get("state") == "off" and set(cloud.hosts) == hosts and own_connector(cove, machines[1]) is None and not own.exists(), f"and Desktop starting on {cove.name} again leaves access off and registers nothing", again)
    check(bay_id in cloud.connected(), f"while {bay.name} stays connected")
    bay.stop()


def main() -> None:
    state = pathlib.Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else None
    if state is not None:
        state.mkdir(parents=True, exist_ok=True)
    base = os.environ.get("HAARNESS_BASE_BIN")
    root = pathlib.Path(tempfile.mkdtemp(prefix="remote-identity-", dir=state))
    cloud = Cloud()
    checks: list[str] = []
    computers: list[Computer] = []

    def check(passed: bool, line: str, detail=None) -> None:
        if not passed:
            raise SystemExit(f"FAIL: {line}: {detail!r}")
        checks.append(f"PASS {line}")
        print(f"PASS {line}", flush=True)

    def computer(*args, **kwargs) -> Computer:
        made = Computer(root, cloud, *args, **kwargs)
        computers.append(made)
        return made

    try:
        old = file_name(cloud, None)
        if os.environ.get("HAARNESS_IDENTITY_RELEASES_ONLY") != "1":
            computers_apart(root, cloud, computer, check, old)
            kept_in_step(root, cloud, computer, check, old, None, ("Bay", "Cove"))

        if not base:
            print("SKIP releases side by side: set HAARNESS_BASE_BIN to the release before", flush=True)
        else:
            # 7. Upgrade and downgrade.
            den = computer("Den", "MACHINE-DEN")
            den.start(base)
            den_id = connected_as(den, cloud)
            check(den.file.name == old and "machine" not in den.identity(), "the release before keeps Den's identity under the old name, unstamped")
            den.stop()
            hosts = set(cloud.hosts)
            den.start()
            check(connected_as(den, cloud) == den_id and den.file.name == old, "after the upgrade Den keeps its host, in the same file")
            check(den.identity().get("machine") == stamp("MACHINE-DEN", den_id), "stamped where it was")
            den.stop()
            den.start(base)
            check(connected_as(den, cloud) == den_id and set(cloud.hosts) == hosts, "going back to the release before keeps the same host, and no new one")
            check(den.identity().get("machine") == stamp("MACHINE-DEN", den_id), "and the stamp stays")
            den.stop()

            # 8. Releases side by side.
            den.start()
            connected_as(den, cloud)
            enable = den.run("remote", "enable", "--name", "Den", binary=base)
            time.sleep(4)
            check(enable.returncode == 0 and "already running" in enable.stdout and set(cloud.hosts) == hosts, "the release before's remote enable beside the new connector says it already runs", enable.stdout + enable.stderr)
            den.stop()
            leaver = computer("Leaver", "MACHINE-LEAVER")
            leaver.start(base)
            leaver_id = connected_as(leaver, cloud)
            pid = leaver.connector.pid
            leaver.cli("account", "--rpc", input=json.dumps({"action": "logout"}))
            wait_for(lambda: leaver.connector.poll() is not None, 30, "the release before's connector to stop at sign-out")
            check(leaver_id not in cloud.connected() and not alive(pid), "signing out stops a connector the release before runs here")

            # 9. A copy the release before made, on a computer of another name.
            annex = computer("Annex", "MACHINE-ANNEX")
            annex.config.mkdir(parents=True, exist_ok=True)
            pre = {k: v for k, v in den.identity().items() if k != "machine"}
            (den.config / old).write_text(json.dumps(pre))
            (annex.config / old).write_text(json.dumps(pre))
            hosts = set(cloud.hosts)
            annex.start()
            check(connected_as(annex, cloud) == den_id and annex.file.name == old and annex.identity().get("machine") == stamp("MACHINE-ANNEX", den_id), "a copy from a computer of another name, made before stamps, keeps its id there", annex.file.name)
            check(annex.status_ids() == [den_id] and set(cloud.hosts) == hosts, "and counts there as that computer's, with no new host", annex.status_ids())
            # Split as REMOTE-ACCESS says: stop it, delete the identity
            # `remote status` lists, start again.
            annex.cli("remote", "disable", den_id)
            annex.stop()
            for path, identity in annex.identities().items():
                if identity.get("id") == den_id:
                    path.unlink()
            annex.start()
            annex_id = connected_as(annex, cloud)
            check(annex_id != den_id and annex_id not in hosts, "split, Annex registers a host of its own", annex_id)
            den.start()
            check(connected_as(den, cloud) == den_id and den.file.name == old, "while Den keeps its host")
            settled = time.time()
            time.sleep(5)
            check(len(cloud.connected() & {den_id, annex_id}) == 2 and not [id for at, id in cloud.takeovers if at >= settled], "both connected, neither taking the other's connection")
            annex.stop()
            den.stop()

            # 10. Renamed computers: the release before ran the connector
            # under a name this computer no longer has.
            nook = computer("Nook", "MACHINE-NOOK")
            nook.start(base, name="Old Name")
            nook_id = connected_as(nook, cloud)
            nook.stop()
            hosts = set(cloud.hosts)
            nook.start(name="Nook Pro")
            check(connected_as(nook, cloud) == nook_id and set(cloud.hosts) == hosts and nook.file.name == old, "with nothing running, the new release's terminal start under another name keeps the host")
            check(nook.identity().get("machine") == stamp("MACHINE-NOOK", nook_id), "stamped where it was")
            nook.stop()
            porch = computer("Porch", "MACHINE-PORCH")
            porch.start(base, name="Old Name")
            porch_id = connected_as(porch, cloud)
            porch.stop()
            hosts = set(cloud.hosts)
            restored = porch.restore()
            pid = wait_for(lambda: porch.identity().get("pid") if alive(porch.identity().get("pid")) else None, 60, "Porch's restored connector")
            check(restored.get("state") == "on" and connected_as(porch, cloud, pid=pid) == porch_id and set(cloud.hosts) == hosts, "with nothing running, Desktop's restore turns access back on with the same host", restored)
            others = sorted(host["name"] for id, host in cloud.hosts.items() if id != porch_id)
            check(porch.identity().get("machine") == stamp("MACHINE-PORCH", porch_id) and porch.others() == others, "stamped, and not listed among its own Your computers", porch.others())
            porch.cli("remote", "disable", porch_id)
            loft = computer("Loft", "MACHINE-LOFT")
            loft.start(base, name="Old Name")
            loft_id = connected_as(loft, cloud)
            hosts = set(cloud.hosts)
            first_restore = loft.restore()
            check(first_restore.get("state") in ("on", "connecting") and loft.identity().get("machine") == stamp("MACHINE-LOFT", loft_id), "Desktop's restore finds it running and makes it this computer's", first_restore)
            loft.stop()
            again = loft.restore()
            restored = connected_as(loft, cloud, pid=wait_for(lambda: loft.identity().get("pid") if alive(loft.identity().get("pid")) else None, 60, "the restored connector"))
            check(again.get("state") == "on" and restored == loft_id and set(cloud.hosts) == hosts, "after it stopped, the next restore turns access back on with the same host", again)
            loft.cli("remote", "disable", loft_id)

            kept_in_step(root, cloud, computer, check, old, base, ("Port", "Quay"))
        (root / "checks.json").write_text(json.dumps({"verdict": "PASS", "checks": checks}, indent=2))
    except BaseException as error:
        (root / "checks.json").write_text(json.dumps({"verdict": "FAIL", "error": str(error), "checks": checks}, indent=2))
        for each in computers:
            if each.log.exists():
                print(f"--- {each.name} connector output\n{each.log.read_text()[-3000:]}", flush=True)
        raise
    finally:
        for each in computers:
            each.stop()
            # Connectors started in the background (`remote enable`, Desktop's
            # restore) name their pid in the identity they run.
            for identity in each.identities().values():
                pid = identity.get("pid")
                if alive(pid) and str(root) in subprocess.run(["ps", "-o", "command=", "-p", str(pid)], capture_output=True, text=True).stdout:
                    os.kill(pid, signal.SIGTERM)
        # A project server a stopped connector was still starting comes up
        # after the first stop, so each is stopped once more a moment later.
        for attempt in range(2):
            if attempt:
                time.sleep(3)
            for each in computers:
                for workspace in (each.project, each.folder / "chat-home"):
                    for binary in filter(None, (BIN, base)):
                        each.run("serve", "--stop", binary=binary, workspace=workspace)
        cloud.loop.call_soon_threadsafe(cloud.loop.stop)
        # A state folder named on the command line is the caller's to keep.
        if state is None:
            e2e_scratch.settle(root)
        else:
            print(f"Results in {root}", flush=True)


if __name__ == "__main__":
    main()
