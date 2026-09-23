# Sign in to Haarness

Run `haarness login`, finish sign-in in your browser, then choose a model under **Haarness Cloud**. For an SSH or headless terminal, use `haarness login --device` and approve the displayed code from another device. `haarness auth login haarness` is an alias for the same account flow.

Desktop has **Sign in to Haarness** in its sidebar and **Use Haarness Cloud** in Connect provider. The account dialog shows available credits and Cloud workspaces, with links to billing and usage. Existing provider keys and self-hosted router connections continue to work independently.

`haarness account` shows the signed-in user and selected account; `haarness account --select <account-id>` switches it. `haarness logout` removes local credentials and revokes the login. Sign-out does not stop running Cloud workspaces; stop them separately to stop compute usage. If Cloud is unreachable during sign-out, credentials are removed locally and the command explains how to revoke the session online.

## Cloud workspaces

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
