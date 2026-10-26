# Marketplace and capabilities

> **User guides:** [Marketplace](https://haarness.ai/docs/cloud/marketplace) and
> [Plugins](https://haarness.ai/docs/cli/plugins). This page is the engineering view.

Haarness can install five kinds of resource on a machine: **skills**, **agents**, **MCP servers**,
**Claude Code plugins** and **extensions**. Two catalogs feed them, and one switchboard turns
installed ones on and off:

| | What it is | CLI |
| --- | --- | --- |
| **Haarness marketplace** | Haarness Cloud's catalog of public and account-private resources, with versions, collections and checksummed artifacts | `haarness marketplace` |
| **Claude Code plugin marketplaces** | Git repositories or folders with `.claude-plugin/marketplace.json`, registered on this machine | `haarness plugin` (see [Plugins, skills and tools](REFERENCE.md#plugins-skills-and-tools)) |
| **Capabilities** | What is installed (from either catalog or written by hand) and whether each one is on | `haarness capability` |

Every client reaches the same code, `ManagementService` in `crates/haarness-cli/src/management/`:
the CLI calls it directly; the TUI (`/marketplace`, alias `/market`), Desktop
(**Settings → Capabilities → Marketplace**) and the SDKs use `POST /v1/manage/{resource}` with
`marketplace`, `capabilities`, or the server-authored pages `marketplace-ui` and `capabilities-ui`;
the model uses the intent-gated `marketplace_manage` and `capability_manage` tools. Changes queue a
`configuration.reload_queued` event and take effect in the next configuration generation, once active
turns are idle. Nothing needs a restart.

## The Haarness marketplace

```sh
haarness marketplace search --kind skill --q review
haarness marketplace detail acme/code-review          # a resource UUID or publisher/slug
haarness marketplace versions acme/code-review
haarness marketplace install skill acme/code-review   # --local for this project, --version N
haarness marketplace install mcp acme/docs --values '{"API_TOKEN":"…"}'
haarness marketplace installed                        # --json for the receipts
haarness marketplace update acme/code-review
haarness marketplace disable acme/code-review         # enable, remove
haarness marketplace request '{"action":"installed"}' # the machine-readable form the SDK uses
```

**Catalog.** Reads go to the Cloud API (`api/v1/marketplace/resources`, `…/collections`,
`…/resources/{id}`, `…/versions`, `…/install-manifest`). Signed in, the request carries the account,
so account-private resources are listed; signed out, only public ones. Inside a Cloud workspace the
runtime reads its owning account's catalog through `HAARNESS_CLOUD_MARKETPLACE_URL` with the workspace's
token.

**Install.** `crates/haarness-cli/src/marketplace.rs` fetches the install manifest, downloads the
artifact (at most 256 MiB, 512 MiB unpacked) and refuses it unless its size and SHA-256 match the
manifest; an extension's npm tarball must also match its `sha512` integrity. Archives are unpacked into
a private staging folder under the receipt root; entries that escape the folder or are links are
refused. The result is validated for its kind, then moved into place in one rename:

| Kind | Installed at (global / `--local`) |
| --- | --- |
| skill | `~/.config/haarness/skills/<name>/` / `.haarness/skills/<name>/` (must hold `SKILL.md`) |
| agent | `~/.config/haarness/agents/<name>.md` / `.haarness/agents/<name>.md` |
| plugin | `~/.config/haarness/plugins/local/<name>/` / `.haarness/plugins/<name>/` (needs `.claude-plugin/plugin.json`) |
| mcp | a plugin bundle `mcp-<name>` in the same place, with the server in its `.mcp.json` and any support files beside it |
| extension | `<data>/extensions/packages/…`, through the extension installer (see [Extensions](EXTENSIONS.md)) |

An MCP resource's `values` fill its configuration placeholders as plain text (never through a shell);
a `cwd` in its configuration is refused, and it reaches its support files as `${CLAUDE_PLUGIN_ROOT}`.
`--local` needs project configuration on (not `--disable-project-config`).

**Receipts.** Each install writes `<id>.json` under `~/.config/haarness/marketplace/` (or
`.haarness/marketplace/` for `--local`) with the resource, version, lock and a digest of the installed
tree. A receipt is honoured only if its path is exactly where its kind installs, so a hand-edited
receipt cannot point an update or removal anywhere else. Update and remove refuse when the installed
files no longer match the digest ("Resource was modified locally; preserve your edits…"), and an
install refuses to overwrite a resource of the same name that it did not install.

## Capabilities

```sh
haarness capability                                       # {"action":"list"}
haarness capability '{"action":"disable","kind":"skill","name":"release-check"}'
haarness capability '{"action":"enable","kind":"mcp","name":"docs","scope":"global"}'
```

`list` returns skills, agents, plugins, MCP servers, extensions and marketplace receipts, each with
`enabled`. Turning a skill, agent or MCP server off never deletes its files: the override is stored in
`capabilities.json` (`~/.config/haarness/capabilities.json`, or `.haarness/capabilities.json` in the
project, which is the default scope) and read by the runtime when it builds the next generation.
Plugins and marketplace MCP bundles are turned off with a `.haarness-disabled` marker in their folder,
which survives version updates; extensions use their own registry. `plugin` and `extension` requests
are forwarded to those services. The state lives in `crates/haarness-cli/src/capability_state.rs`.

## Tests

- `cargo test -p haarness-cli marketplace` covers verification, layouts, collisions, edits kept on
  update and archives that try to escape.
- `HAARNESS_BIN=target/debug/haarness python3 scripts/test-marketplace.py` runs a fixture catalog
  through the CLI, API and tools to a live runtime, with real ZIP and npm installs and hot reload.
- `scripts/test-real-plugins.sh` installs real plugins from Anthropic's public Claude Code marketplace
  (networked, optional).
- Cloud-side catalog, review and storage checks are in the
  [marketplace testing skill](../skills/haarness-marketplace-capabilities/SKILL.md).
