# Remote computers through Haarness Cloud

Use your own machine from Cloud web, the Cloud API, or the Python and TypeScript SDKs. The machine runs Haarness and makes an outbound authenticated WebSocket connection. No inbound port, SSH server, tunnel or Apple service is needed.

```sh
haarness login
cd /path/to/project
haarness remote enable --name "My computer"
haarness remote status
```

Open **Remote hosts** in Cloud. Browse/open a folder, select a project, and work with its independent chats, shared files, approvals, model selection and terminal. The connector recovers when its local daemon restarts or changes address. It uses the existing registered project daemons; it does not rent compute or copy the project into Cloud. Model calls still follow the provider and billing configuration on that computer.

`remote run` runs in the foreground and can be supervised by an OS service. `enable` starts a background process for the current login, not a boot/login startup service. Keep the machine awake. `remote disable HOST_ID` stops its connector without stopping accepted local work. `remote revoke HOST_ID` removes Cloud access. `remote forget HOST_ID` revokes and forgets a stopped local identity, allowing an explicit fresh registration. Host removal and project closing retain files and histories.

To expose one conversation tree instead of project/terminal controls:

```sh
haarness remote run --chat CHAT_ID --name "Shared investigation"
```

Find the native chat ID in its URL or `haarness sessions list --json`. Published chats include descendants and the project filesystem shared by all its chats; this is not a filesystem sandbox. The remote caller cannot enumerate/read/control other chat histories or host terminals. The machine checks this scope independently of the Cloud relay, and refuses publishing through an older runtime without scope enforcement.

## SDKs

```typescript
const cloud = new HaarnessCloud({ baseUrl: "https://app.haarness.dev", token });
const hosts = await cloud.listRemoteHosts();
const host = cloud.remoteHost(hosts.items[0].id);
const projects = await host.hostProjects();
const runtime = host.workspace(projects.workspaces[0].id);
const chat = await runtime.createThread({ provider: "haarness", model: "openai/gpt-4.1-nano" });
const result = await runtime.chat(chat.id, "Explain this project");
// cloud.remoteSession(publicationId) provides the scoped runtime directly.
// Native /v1 methods, events(), files and connectPty() use the relay.
runtime.close(); host.close(); cloud.close();
```

Python uses `remote_host(id).workspace(project_id)` and `remote_session(id)` on both `HaarnessCloud` and `AsyncHaarnessCloud`. CRUD methods for host/publication metadata are generated from the Cloud OpenAPI contract. Runtime credentials are resolved afresh for HTTP, events and PTYs; they never fall back to the local server token. Close runtime clients/streams when no longer needed. Native `/healthz` and `/openapi.json` are local discovery endpoints; remote runtime methods use `/v1`.

See the Cloud repository's `docs/REMOTE-ACCESS.md` for authorization, routing and deployment details and `skills/haarness-remote-access/SKILL.md` for verification.
