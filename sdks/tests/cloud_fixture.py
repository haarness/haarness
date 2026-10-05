"""Isolated fixture-only OAuth + test Stripe funding for SDK integration tests."""

import base64
import hashlib
import re
import secrets
import time
from urllib.parse import urlparse, parse_qs
import httpx


def authorize(base, hint):
    assert urlparse(base).hostname in ("localhost", "127.0.0.1"), (
        "Fixture tests require loopback"
    )
    http = httpx.Client(timeout=30, follow_redirects=False)
    start = http.get(base + "/auth/google/start", params={"return_to": "/app"})
    location = start.headers["location"]
    assert urlparse(location).hostname in ("localhost", "127.0.0.1")
    reply = http.get(location + "&login_hint=" + hint)
    callback = reply.headers["location"]
    assert callback.startswith(base + "/auth/google/callback")
    assert http.get(callback).status_code == 302
    verifier = secrets.token_urlsafe(48)
    challenge = (
        base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest())
        .decode()
        .rstrip("=")
    )
    redirect = "http://127.0.0.1:49152/callback"
    page = http.get(
        base + "/oauth/authorize",
        params={
            "response_type": "code",
            "client_id": "haarness-cli",
            "redirect_uri": redirect,
            "scope": "account workspaces billing keys offline_access",
            "state": secrets.token_urlsafe(16),
            "code_challenge": challenge,
            "code_challenge_method": "S256",
        },
    )
    page.raise_for_status()
    request = re.search(r'data-request-id="([^"]+)"', page.text).group(1)
    approved = http.post(
        base + "/oauth/authorize",
        json={"requestId": request, "decision": "approve"},
        headers={"X-CSRF-Token": http.cookies.get("hc_csrf")},
    )
    approved.raise_for_status()
    code = parse_qs(urlparse(approved.json()["redirectUri"]).query)["code"][0]
    tokens = http.post(
        base + "/oauth/token",
        data={
            "grant_type": "authorization_code",
            "client_id": "haarness-cli",
            "redirect_uri": redirect,
            "code_verifier": verifier,
            "code": code,
        },
    )
    tokens.raise_for_status()
    result = tokens.json()
    result["apiUrl"] = base
    http.close()
    return result


def fund(cloud):
    wallet = cloud.get_wallet()
    if wallet["includedAi"]["nanos"] != "0":
        return
    checkout = cloud.billing_checkout(
        {"kind": "subscription", "planCode": "pro"},
        options={"headers": {"Idempotency-Key": secrets.token_hex(16)}},
    )
    url = checkout["checkoutUrl"]
    parsed = urlparse(url)
    assert parsed.hostname in ("localhost", "127.0.0.1") and "/cs_fx" in parsed.path, (
        "Only fixture payments may be confirmed"
    )
    with httpx.Client() as http:
        result = http.post(
            f"{parsed.scheme}://{parsed.netloc}/fixture/checkout/{parsed.path.rsplit('/', 1)[1]}/confirm?duplicate=fresh"
        )
        result.raise_for_status()
    for _ in range(60):
        if cloud.get_wallet()["includedAi"]["nanos"] != "0":
            entitlement = cloud.billing_entitlements()
            subscription = cloud.get_subscription()
            assert entitlement["planCode"] == subscription["planCode"] == "pro"
            assert entitlement["aiCredits"] == 8_000 and entitlement["cloudUnits"] == 40
            return
        time.sleep(0.25)
    raise AssertionError("Fixture credit grant timed out")
