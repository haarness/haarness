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

Create/start/stop return once the operation is accepted. Use `list` to check its state. Desktop provides the same actions in the account dialog. Saved Cloud projects contain account/workspace identifiers; attach tickets are acquired again when opened and renewed while connected. Workspace access through a revoked native login is rejected within the gateway's existing ticket-cache window (10 seconds by default). Existing open connections follow the gateway's stream policy.

## Session storage and development

CLI and Desktop use the same account client: PKCE with a loopback callback, or device authorization, rotating refresh tokens, a single retry after a rejected access token, and an OS file lock across processes. Credentials are written atomically with owner-only permissions. Account refresh tokens are never passed to a workspace, renderer, project configuration or model provider upstream.

The default issuer is `https://app.haarness.dev`. Credentials use `~/.config/haarness-cloud` (or `$XDG_CONFIG_HOME/haarness-cloud`), shared with `haarness-cloud`. An explicit `HAARNESS_CLOUD_CONFIG_DIR` overrides this; when `HAARNESS_DATA_DIR` is set, native Haarness defaults to its `config/account` directory for isolated development. Set the explicit Cloud config directory when mixing both CLIs in such an isolated environment. `--data-dir` changes runtime data, not the global account.

For local development, set `HAARNESS_CLOUD_API_URL` to the API's loopback URL and optionally `HAARNESS_CLOUD_WEB_ORIGIN` when the web UI is on a separate origin. Only HTTPS and loopback HTTP are accepted for account/router traffic; token-bearing requests do not follow redirects.

The API exchanges an authenticated native session for a five-minute router credential. The router checks the login family, current membership, billing status and model restrictions. Automatic refresh does not create a user-visible developer API key. Explicit developer keys retain their own lifecycle.

Deploy the Cloud migration and API/router changes before distributing the native client. No production configuration or deployment is performed by the local test suite.
