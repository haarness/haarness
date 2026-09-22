# Connect Haarness Router

Haarness Router connects your model providers through one endpoint. The
self-hosted integration is available in Desktop, TUI, and CLI.

## Desktop and TUI

In Desktop, open **Settings → Providers → Connect Haarness Router**.
In the terminal, use **/providers → Connect Haarness Router**.

Enter your router address and choose **Connect**. New self-hosted routers need
no key by default. If yours requires authentication, Haarness asks for its
router access key. Wrong keys and unreachable endpoints leave your
configuration unchanged.

Use **Advanced** to give the connection a name or limit it to the current
trusted project. You can add multiple named routers. The model picker loads
current models automatically, including the router's aliases.

When attached to a remote server, the connection is checked and saved on that
server; `localhost` means the server's machine. Provider keys remain on the
router. A router access key, if required, is saved in the Haarness server's
credential store. The router connection becomes available automatically when
active work is idle.

## CLI

Run on the machine hosting the Haarness server:

```sh
haarness provider connect-router http://localhost:8080
haarness provider connect-router https://router.example --name "Team router"
haarness --trust-project provider connect-router http://localhost:8080 --project
```

For a protected router, supply the key through stdin with `--key-stdin`; obtain
it from your secret manager or router owner. It is never accepted as a command
argument or written into provider configuration.

Then choose a model in the picker or use its qualified CLI name:

```sh
haarness run --model router/openai/gpt-6-astra "Hello"
haarness run --attach http://server.example:4096 \
  --model router/fast "Hello"
```

The first connection gets ID `router`; additional connections get independent
IDs such as `router_2`. Their names and endpoints can change without replacing
that identity. Use `haarness provider list` to inspect your connections.

## Router authentication

The sibling router's prepared launcher defaults to no router-key requirement:

```sh
./scripts/run-stackforward
# Opt into a generated local access key:
./scripts/run-stackforward --require-key
```

The key is prepared in the router's private `.env`. A plain router binary uses
`HAARNESS_ROUTER_KEY` with `--require-key`. Existing configurations that already
list key environment variables stay protected. `--no-auth` explicitly disables
the requirement. Web UI login and upstream provider credentials are separate.

## Extension point for Cloud

Connection configuration persists an integration identity independently of the
API protocol and credentials:

```toml
[providers.router.integration]
kind = "haarness-router"
deployment = "self-hosted"
```

The shared connection operation owns address normalization, reachability/auth
checks and model discovery validation. Cloud can add a deployment and account
login flow that produces a compatible connection without changing conversation
model routing. Only the available self-hosted flow is shown today.

## Verification

Focused Rust connection tests cover URL normalization, no-key setup, required
keys, rejected-key recovery, independent connection IDs, dynamic models and
credential separation. Desktop Electron tests exercise the actual wizard; the
opt-in live case is enabled with `HAARNESS_ROUTER_LIVE_URL`.

The sibling router's `scripts/test-haarness-attach.py` starts temporary router
and Haarness servers, connects through the shared management operation, runs
live tools and continuation, and exercises a real tmux connection wizard plus
CLI setup. Run with `HAARNESS_ROUTER_AUTH=required` for the protected-router case.
