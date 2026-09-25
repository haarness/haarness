# Sign in to Haarness

Run `haarness login`, finish sign-in in your browser, then choose a model under **Haarness Cloud**. For an SSH or headless terminal, use `haarness login --device` and approve the displayed code from another device. `haarness auth login haarness` is an alias for the same account flow.

Desktop has **Sign in to Haarness** in its sidebar and **Use Haarness Cloud** in Connect provider. The account dialog shows available credits and Cloud sessions, with links to billing and usage. Existing provider keys and self-hosted router connections continue to work independently.

Signing in to OpenAI with ChatGPT connects the **OpenAI** provider. Choosing
**Haarness Cloud** uses the signed-in Haarness account and Cloud's upstream API
credentials. TUI and Desktop share both credential stores when they use the same
profile; a ChatGPT subscription's model access and quota do not transfer to
Cloud's API account.

Cloud model capabilities come from the routed provider's catalog. The model
picker shows its supported reasoning levels, and `/reasoning` exposes the same
choices in TUI. A non-reasoning model has no reasoning slider. Tools, images,
context limits and opaque reasoning history follow the provider contract; a
public Cloud model ID can differ from its upstream ID.

For an ordinary turn, the client lets Cloud choose an output allowance instead
of requesting the model's full output capacity. Cloud caps that allowance at
32,768 tokens, the model ceiling, and its available pending-request monetary
allowance. An
explicit agent/extension output limit is still honored subject to admission.
Cloud reserves and forwards the same bound, then settles actual provider usage.
An exhausted pending-request allowance is reported separately from upstream
provider quota. Provider outages, retired models and API-account restrictions
can still prevent a listed model from serving a request.

`haarness account` shows the signed-in user and selected account; `haarness account --select <account-id>` switches it. `haarness logout` removes local credentials and revokes the login. Sign-out does not stop running Cloud sessions; stop them separately to stop compute usage. If Cloud is unreachable during sign-out, credentials are removed locally and the command explains how to revoke the session online.

## Cloud agents and sessions

A cloud agent is a reusable template containing instructions, model/reasoning defaults,
repository, SSH deploy key, pinned hosts, environment, startup command and runtime size.
Each session starts an independent Fly runtime with its own persistent files and conversations.
Editing a template affects future sessions; existing sessions retain their captured version.
Templates have no compute charge. Running sessions use your plan's concurrency and compute allowance;
stopped sessions retain billable storage.

Create a template in the Cloud website's **Cloud agents** page, or pass its JSON definition
to `haarness cloud agent create --file agent.json`. Environment values are write-only;
the returned public deploy key can be added to your repository.

```sh
haarness cloud agent list
haarness cloud agent show AGENT_ID
haarness cloud session create --agent AGENT_ID --title "Investigate issue" --json
haarness cloud session list
haarness cloud session attach SESSION_ID
haarness cloud session stop SESSION_ID
haarness cloud session start SESSION_ID
```

Create/start/stop return once accepted. Wait for `running` before attaching. Attach resumes the
most recent conversation. Desktop offers **Haarness account → Cloud sessions → New session**
with a template picker; **Manage agent templates** opens the browser editor. New conversations
honor the runtime's default agent and model in both TUI and Desktop.

Existing workspaces appear as sessions with their files, conversations and old URLs intact.
The previous CLI commands remain available for compatibility:

```
haarness cloud workspace list
haarness cloud workspace create my-workspace
haarness cloud workspace create my-repo --repo https://github.com/owner/repo.git
haarness cloud workspace start <id>
haarness cloud workspace attach <id>
haarness cloud workspace stop <id>
```

### Ask your assistant to use Cloud

After `haarness login`, the same tools are available to models in CLI, TUI and Desktop:

- `cloud_agent_manage`: find, inspect, create, update and delete templates, discover Cloud models,
  and retrieve a template's public SSH deploy key.
- `cloud_agent_session`: launch independent sessions, chat and read results, wait for a turn,
  interrupt, rename, archive, stop, resume or delete a session.

For example, ask: "Find my Repository helper cloud agent. Launch a session from it, ask it
to inspect the repository, and bring back the result." Follow up with "Ask that cloud session
to run the tests" or "Stop that cloud session and keep its files." The assistant returns a
session link you can open in Cloud to continue the same conversation and inspect files.

You can also use a normal CLI run (with any connected tool-capable model):

```sh
haarness login
haarness run --provider haarness --model openai/gpt-5-mini \
  "List my cloud agents and sessions. Do not start anything."
```

`--model haarness-cloud/openai/gpt-5-mini` is the equivalent fully qualified form.
With `--provider haarness`, upstream names such as `openai/...` remain Cloud model IDs.

Signing into a model provider alone does not sign into Haarness Cloud. These tools require
your existing Haarness login and selected account. They never start a login flow or accept
tokens, issuer URLs, or credential paths from the model. Use the Cloud editor/API to configure
write-only environment secrets. Ordinary tool permission rules also apply; for example,
`cloud_agent_session:ask:*` controls remote chat. The plan agent can inspect templates,
sessions and history but cannot launch or mutate them, even with an allow override.

The tools are discoverable under **Cloud agents** in the tool catalog and can be loaded with
`load_tool` when lazy loading is enabled. Reads never start a stopped runtime. `ask` without
`sessionId` creates one from `cloudAgentId`; follow-ups use the returned session/thread IDs.
`wait=false` submits the message after startup and returns without waiting for the reply.
Waits default to 120 seconds (maximum 600). A startup timeout reports `messageSubmitted:false`;
retry `ask` with that session ID once it is running. A running turn at timeout continues remotely:
use `wait` with its IDs instead of submitting another message. History supports `after`/`nextAfter`
cursors and `limit` (1–100); without `after`, it returns the latest page.

Remote permission prompts remain user approvals in the session's Cloud chat. The tool returns
`needs_attention` and its URL instead of approving them. Cancelling a synchronous ask attempts
to interrupt only the turn it submitted; the session and files remain. Cancelling an observation
does not interrupt someone else's work. Archive hides a session without stopping it; stop releases
compute while retaining storage, and delete removes the session and its disk.

Create/start/stop return once the operation is accepted. Use `list` to check its state. Desktop provides the same actions in the account dialog. Saved Cloud projects contain account/workspace identifiers; attach tickets are acquired again when opened and renewed while connected. Workspace access through a revoked native login is rejected within the gateway's existing ticket-cache window (10 seconds by default). Existing open connections follow the gateway's stream policy.

## Session storage and development

CLI and Desktop use the same account client: PKCE with a loopback callback, or device authorization, rotating refresh tokens, a single retry after a rejected access token, and an OS file lock across processes. Credentials are written atomically with owner-only permissions. Account refresh tokens are never passed to a workspace, renderer, project configuration or model provider upstream.

The default issuer is `https://app.haarness.dev`. Credentials use `~/.config/haarness-cloud` (or `$XDG_CONFIG_HOME/haarness-cloud`), shared with `haarness-cloud`. An explicit `HAARNESS_CLOUD_CONFIG_DIR` overrides this; when `HAARNESS_DATA_DIR` is set, native Haarness defaults to its `config/account` directory for isolated development. Set the explicit Cloud config directory when mixing both CLIs in such an isolated environment. `--data-dir` changes runtime data, not the global account.

For local development, set `HAARNESS_CLOUD_API_URL` to the API's loopback URL and optionally `HAARNESS_CLOUD_WEB_ORIGIN` when the web UI is on a separate origin. Only HTTPS and loopback HTTP are accepted for account/router traffic; token-bearing requests do not follow redirects.

The API exchanges an authenticated native session for a five-minute router credential. The router checks the login family, current membership, billing status and model restrictions. Automatic refresh does not create a user-visible developer API key. Explicit developer keys retain their own lifecycle.

Deploy the Cloud migration and API/router changes before distributing the native client. No production configuration or deployment is performed by the local test suite.

## Independently configured Cloud sessions

Templates are optional. `haarness cloud session create --title "My work"` starts a blank session;
`--file session.json` accepts a creation body with `configuration` (instructions, model/reasoning,
MCP, agents, skills, permissions, repository/setup/resources) and optional write-only `environment`.
Add `--agent ID` to inherit a template and override supplied fields, or `--stopped` to prepare SSH access.

Stop and wait for a session to be stopped, then use `cloud session update ID --file settings.json`
with its current `version` and changed `configuration`. `cloud session save-agent ID --version N
--name "Template"` saves its last saved settings as an inert template with a fresh SSH key; environment
is copied only with `--include-environment`. Restart applies the saved settings while retaining files/chat.

The `cloud_agent_session` model tool supports inline `configuration` without `cloudAgentId`, plus
`update`, `save_agent` and `ssh_public_key`. It never reads or sets environment secret values.
Desktop offers blank/template creation, instructions and advanced JSON settings; its session Settings
button opens the shared Cloud editor. CLI/TUI/Desktop continue to share the account login.
