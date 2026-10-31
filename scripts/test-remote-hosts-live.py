#!/usr/bin/env python3
"""Your other computers in Desktop, against a real local Haarness Cloud.

  HAARNESS_REMOTE_E2E=1 HAARNESS_CLOUD_API_URL=http://127.0.0.1:19080 \
    scripts/test-remote-hosts-live.py [state-folder]

HAARNESS_BIN picks the binary (default target/debug/haarness). The Cloud must
be a local one with the fixture identity provider (its native dev stack): the
script signs one fixture user in four times with the device flow, approving
each code with the user's web session, so no credential is typed or printed.

1. Three computers share themselves with `haarness remote enable`, each with
   its own data folder, sign-in and project (echo provider): Studio, Old box
   and Desk. Old box stops its connector again, so Cloud lists it as away.
2. Desk is the computer Desktop runs on: Desktop gets Desk's data folder, so
   Desk's own connector must never be listed.
3. The account bridge lists Studio and Old box, never Desk, and hands out the
   account's sign-in for the relay.
4. Desktop Playwright (e2e/remote-hosts.spec.ts, "live") opens Studio from the
   switcher, chats on it, runs a command in its terminal, opens another of its
   folders through its folder browser, and checks Old box is shown away.
5. Every connector stops, every host is revoked, every project server stops.

Writes secret-free notes and the Desktop screenshots under the state folder.
"""

import http.cookiejar, json, os, pathlib, re, shutil, subprocess, sys, time
import urllib.error, urllib.parse, urllib.request

assert os.environ.get("HAARNESS_REMOTE_E2E") == "1", "Set HAARNESS_REMOTE_E2E=1"
API = os.environ["HAARNESS_CLOUD_API_URL"].rstrip("/")
assert urllib.parse.urlsplit(API).hostname in ("127.0.0.1", "localhost"), "Only a local test Cloud is allowed"
REPO = pathlib.Path(__file__).resolve().parents[1]
BIN = str(pathlib.Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve())
ROOT = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else REPO / "target" / f"remote-hosts-e2e-{os.getpid()}").resolve()
CONFIG = '[defaults]\nprovider="echo"\nmodel="echo-1"\n[lsp]\nenabled=false\n'
HINT = f"remote-hosts-{os.getpid()}"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


jar = http.cookiejar.CookieJar()
web = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar), NoRedirect)


def fetch(url, data=None, headers=None, method=None):
    request = urllib.request.Request(url, data=data, headers=headers or {}, method=method)
    try:
        response = web.open(request, timeout=30)
    except urllib.error.HTTPError as error:
        response = error
    return response.status, response.headers, response.read().decode()


def sign_in_web():
    """The fixture user's browser session: Google start, fixture consent, callback."""
    status, headers, _ = fetch(f"{API}/auth/google/start?return_to=/app")
    assert status in (302, 303, 307), status
    authorize = headers["Location"] + "&login_hint=" + HINT
    status, headers, body = fetch(authorize)
    if status == 200:
        request_id = re.search(r'name="request_id" value="([^"]+)"', body).group(1)
        form = urllib.parse.urlencode({"request_id": request_id, "decision": "approve"}).encode()
        status, headers, body = fetch(authorize, form, {"Content-Type": "application/x-www-form-urlencoded"})
    assert status in (302, 303, 307), status
    status, headers, _ = fetch(headers["Location"])
    assert status in (302, 303, 307) and any(cookie.name == "hc_session" for cookie in jar), status


def csrf():
    return next(cookie.value for cookie in jar if cookie.name == "hc_csrf")


def sign_in_cli(env):
    """`haarness login --device`, approved with the web session."""
    login = subprocess.Popen([BIN, "login", "--device", "--json", "--no-browser"], env=env,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    line = json.loads(login.stdout.readline())
    status, _, body = fetch(f"{API}/api/v1/device/{urllib.parse.quote(line['code'])}/approve", b"",
                            {"x-csrf-token": csrf(), "Content-Type": "application/json"}, "POST")
    assert status == 204, (status, body)
    assert login.wait(timeout=120) == 0, login.stderr.read()


def computer(name):
    folder = ROOT / name
    for sub in ("home", "data", "account", "project"):
        (folder / sub).mkdir(parents=True, exist_ok=True)
    (folder / "project/.haarness").mkdir(exist_ok=True)
    (folder / "project/.haarness/config.toml").write_text(CONFIG)
    (folder / "project/README.md").write_text(f"# {name} project\n")
    env = {**os.environ, "HAARNESS_HOME": str(folder / "home"), "HAARNESS_DATA_DIR": str(folder / "data"),
           "HAARNESS_CLOUD_CONFIG_DIR": str(folder / "account"), "HAARNESS_CLOUD_API_URL": API,
           "HAARNESS_CLOUD_WEB_ORIGIN": API, "HAARNESS_TEST_PROVIDER": "1", "SHELL": "/bin/sh"}
    env.pop("HAARNESS_API_KEY", None)
    return {"name": name, "folder": folder, "env": env, "project": folder / "project"}


def cli(machine, *args, input=None):
    result = subprocess.run([BIN, "--workspace", str(machine["project"]), *args], env=machine["env"],
                            input=input, capture_output=True, text=True, timeout=120)
    assert result.returncode == 0, (args, result.stderr)
    return result.stdout


def wait(what, check, timeout=60):
    end = time.time() + timeout
    while time.time() < end:
        value = check()
        if value:
            return value
        time.sleep(0.5)
    raise SystemExit(f"FAIL: timed out waiting for {what}")


def hosts(machine):
    """What the account bridge lists for Desktop on `machine`."""
    return json.loads(cli(machine, "account", "--rpc", input=json.dumps({"action": "remote-hosts"})))["items"]


def registered(machine):
    """Every host of the account, as Cloud has them."""
    return json.loads(cli(machine, "remote", "hosts", "--json"))["items"]


def main():
    shutil.rmtree(ROOT, ignore_errors=True)
    ROOT.mkdir(parents=True)
    notes = []
    machines = [computer(name) for name in ("Studio", "Old box", "Desk")]
    studio, old, desk = machines
    (studio["folder"] / "project/second").mkdir()
    (studio["folder"] / "project/second/.haarness").mkdir()
    (studio["folder"] / "project/second/.haarness/config.toml").write_text(CONFIG)
    sign_in_web()
    try:
        for machine in machines:
            sign_in_cli(machine["env"])
            cli(machine, "remote", "enable", "--name", machine["name"])
            machine["id"] = wait(f"{machine['name']}'s connector", lambda m=machine: next(
                (c["id"] for c in json.loads(cli(m, "remote", "status", "--json"))["items"] if c.get("running")), None))
        wait("Cloud to list every computer connected", lambda: all(
            any(h["id"] == m["id"] and h["connected"] for h in registered(studio)) for m in machines), 90)
        cli(old, "remote", "disable", old["id"])
        wait("Old box to be away", lambda: any(h["id"] == old["id"] and not h["connected"] for h in registered(studio)), 90)
        listed = hosts(desk)
        names = sorted(h["name"] for h in listed)
        assert names == ["Old box", "Studio"], names
        notes.append(f"PASS bridge lists {names}, never Desk")
        auth = json.loads(cli(desk, "account", "--rpc", input=json.dumps({"action": "remote-auth"})))
        assert auth["token"] and auth["apiUrl"].rstrip("/") == API
        notes.append("PASS bridge hands out the relay sign-in for this Cloud")

        desktop = {k: v for k, v in desk["env"].items() if k not in ("ELECTRON_RUN_AS_NODE", "ELECTRON_RENDERER_URL")}
        desktop.update({
            "HAARNESS_BIN": BIN,
            "HAARNESS_REMOTE_HOSTS_API": API,
            "HAARNESS_REMOTE_HOSTS_ACCOUNT": desk["env"]["HAARNESS_CLOUD_CONFIG_DIR"],
            "HAARNESS_REMOTE_HOSTS_DATA": desk["env"]["HAARNESS_DATA_DIR"],
            "HAARNESS_REMOTE_HOSTS_HOME": desk["env"]["HAARNESS_HOME"],
            "HAARNESS_REMOTE_HOSTS_PROJECT": str(studio["project"]),
            "HAARNESS_REMOTE_HOSTS_SHOTS": os.environ.get("HAARNESS_REMOTE_HOSTS_SHOTS", str(ROOT / "screenshots")),
        })
        result = subprocess.run(["npx", "playwright", "test", "e2e/remote-hosts.spec.ts", "-g", "live", "--reporter=list"],
                                cwd=REPO / "apps/desktop", env=desktop, timeout=900)
        assert result.returncode == 0, "Desktop live test failed"
        notes.append("PASS Desktop opened Studio through the relay, chatted, ran its terminal and opened another folder")
    finally:
        for machine in machines:
            if machine.get("id"):
                subprocess.run([BIN, "--workspace", str(machine["project"]), "remote", "disable", machine["id"]],
                               env=machine["env"], capture_output=True, timeout=60)
                subprocess.run([BIN, "--workspace", str(machine["project"]), "remote", "forget", machine["id"]],
                               env=machine["env"], capture_output=True, timeout=60)
            for project in (machine["project"], machine["project"] / "second"):
                if project.exists():
                    subprocess.run([BIN, "--workspace", str(project), "serve", "--stop"], env=machine["env"],
                                   capture_output=True, timeout=60)
        (ROOT / "checks.json").write_text(json.dumps(notes, indent=2))
    remaining = [h["name"] for h in registered(studio)]
    assert not any(name in ("Studio", "Old box", "Desk") for name in remaining), remaining
    notes.append("PASS every host revoked")
    (ROOT / "checks.json").write_text(json.dumps(notes, indent=2))
    print("\n".join(notes))


if __name__ == "__main__":
    main()
