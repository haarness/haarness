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
haarness marketplace auto-update acme/code-review     # --off to stay on this version
haarness marketplace check-updates                    # install what is new now
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

**Automatic updates.** A receipt's `autoUpdate` says whether the install follows new versions. An
install of the latest version gets `true`; one that names its version (`--version`, or the version
field of a request) gets `false`, and so does `install --no-auto-update`. An update keeps the value it
finds unless the request sets `autoUpdate`. A receipt without the field, which every install made
before this setting has, reads as `false`, so nothing changes on its own on a machine that upgrades.
`auto-update` (the `auto_update` request: `id`, `scope`, `enabled`) rewrites only the receipt.

`check-updates` (the `check_updates` request, optionally with a `scope`) reads each following
resource's detail (`…/resources/{id}`). When its `activeVersion` is another version, approved and not
yanked, it installs that exact version number, so a version published meanwhile is never installed
unseen. It never replaces an install whose files were edited: the receipt gets `"updateHeld":
"modified"`, which goes away once the update succeeds, the edits are undone or automatic updates are
turned off. A resource the catalog does not answer for (offline, signed out, removed) is left for the
next round. The answer is `{checked, updated, held, failed}`, and one `configuration.reload_queued`
event follows a round that changed anything. In **Installed resources** each row says how it gets new
versions (`updates automatically`, `pinned to version 3`, or `has local edits, so automatic updates
wait`) and offers one entry to change it beside **Update**.

**In the background.** A long-running server (`haarness serve`, so the server the TUI and Desktop
start, and Cloud workspace runtimes; never a one-shot command such as `haarness run`) runs a round
about two minutes after it starts and then every `HAARNESS_AUTO_UPDATE_INTERVAL_SECS` seconds (1800 by
default, at least 60; a shorter interval brings the first round forward too). `HAARNESS_AUTO_UPDATE=0`
turns the rounds off. Several servers run on one computer, one per folder, so a stamp decides which of
them does a round: `.auto-update-checked` in the data folder for installs for all projects and the Git
plugin marketplaces below, and the same file in a project's `.haarness/marketplace/` (written only
where that project has installs that follow new versions) for the project's own. The stamp is read
and written under an editor lock, and the configuration watcher skips it. A round never delays
startup, says it could not reach the network at info level, and leaves running turns alone: what it
installs reaches the runtime with the next idle configuration reload.

**Git plugin marketplaces.** `marketplaces.json` stays a plain `name → path` map, which older
versions read. Beside it, `<data>/plugins/marketplace-sources.json` records where each Git clone came
from: `name → {source, autoUpdate, commit, updatedAt}`. Registering a Git marketplace writes its entry
with `autoUpdate: true`; a folder gets none, because it is read as it is. `haarness plugin marketplace
update [name]` (the `update_plugin_marketplace` request, or plugin action `marketplace_update`) asks
the repository for its default branch's commit with `git ls-remote`, using the same non-interactive git
as cloning, and does nothing more when the clone and the record already hold it. Otherwise it clones
into a staging folder beside the old clone, refuses the result if the marketplace now gives itself
another name, swaps the folders (the old one comes back if the swap fails) and then updates each
installed plugin of that marketplace whose files would change, keeping it on or off as it was. A
plugin the marketplace no longer lists stays installed as it is and is reported. A marketplace
registered before sources were recorded has no entry and is not refreshed in the background; updating
it by hand works when its clone has an `origin`, and records an entry with `autoUpdate: false`.
`haarness plugin marketplace auto-update <name> [--off]` (the `plugin_marketplace_auto_update` request
with `name` and `enabled`, or plugin action `marketplace_auto_update` with `name`, `name:on` or
`name:off`) changes that setting. `plugin_marketplaces` lists `type` (`git` or `folder`), `source` (a
credential in it shown as `<redacted>`), `autoUpdate`, `commit` and `updatedAt` for each one. Desktop's
**Settings → Plugins** lists them under **Marketplaces**, with **Update automatically** and **Update**
for a Git one.

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
  update, archives that try to escape, and automatic updates against a stand-in catalog (new, pinned,
  edited, yanked and unreachable resources). `cargo test -p haarness-cli plugins` refreshes Git
  marketplaces from local repositories that get new commits; `auto_update` covers the stamp.
- `HAARNESS_BIN=target/debug/haarness python3 scripts/test-marketplace.py` runs a fixture catalog
  through the CLI, API and tools to a live runtime, with real ZIP and npm installs and hot reload, then
  publishes new versions and waits for a server with `HAARNESS_AUTO_UPDATE_INTERVAL_SECS=60` to install
  them and to fetch a Git plugin marketplace's new commit (about a minute).
- `scripts/test-real-plugins.sh` installs real plugins from Anthropic's public Claude Code marketplace
  (networked, optional).
- Cloud-side catalog, review and storage checks are in the
  [marketplace testing skill](../skills/haarness-marketplace-capabilities/SKILL.md).
