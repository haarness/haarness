# Remote computers through Haarness Cloud

> **User guide:** [Remote control](https://haarness.ai/docs/cloud/remote-hosts). This page has the engineering detail.

Use your own machine from Cloud web, the Cloud API, or the Python and TypeScript SDKs. The machine runs Haarness and makes an outbound authenticated WebSocket connection. No inbound port, SSH server, tunnel or Apple service is needed.

```sh
haarness login
cd /path/to/project
haarness remote enable --name "My computer"
haarness remote status
```

Open **Remote control › Hosts** in Cloud. Browse/open a folder, select a project, and work with its independent chats, shared files, approvals, model selection and terminal. The connector recovers when its local daemon restarts or changes address. It reconnects after a second when Cloud drops a connection that was up for a minute or carried a request; failed connects back off up to 30 seconds, and after four connections in a row that Cloud accepts and drops at once it waits 1, 2, 4, 8 and then 10 minutes between attempts. If Cloud announces a relay protocol this version does not speak, the connector stops and says to update Haarness; `/remote-control` and `remote status` show that reason until the connector starts again. It uses the existing registered project daemons; it does not rent compute or copy the project into Cloud. Model calls still follow the provider and billing configuration on that computer.

`remote run` runs in the foreground and can be supervised by an OS service. `enable` starts a background process for the current login, not a boot/login startup service. Keep the machine awake. `remote disable HOST_ID` stops its connector without stopping accepted local work. `remote revoke HOST_ID` removes Cloud access. `remote forget HOST_ID` revokes and forgets a stopped local identity, allowing an explicit fresh registration. Host removal and project closing retain files and histories.

To expose one conversation tree instead of project/terminal controls, type `/remote-control` (alias `/rc`) in that chat in the TUI or Desktop. The server's `remote-control` management resource (`start`, `stop`, `view`, `status`, each with `threadId`) starts the same background connector as `remote enable --chat`, waits until Cloud has published the chat (listed under **Remote control › Sessions**; the connector keeps its Cloud title in step with the chat's), and returns its link (`/app/remote/sessions/<publication>` on `HAARNESS_CLOUD_WEB_ORIGIN`, else the API origin) with a QR code as rows of modules. `/remote-control stop` stops the connector, unpublishes the chat, revokes its chat-only host and forgets the identity, so sharing again gets a fresh registration and link; sharing stopped from Cloud is detected the same way. From a shell:

```sh
haarness remote enable --chat CHAT_ID --name "Shared investigation"   # background
haarness remote run --chat CHAT_ID --name "Shared investigation"      # foreground
```

Find the native chat ID in its URL or `haarness sessions list --json`. Attachments sent from Cloud clients may be images, PDFs, audio, video or text files (text is read into the message); other files are refused before the message is stored. Published chats include descendants and the project filesystem shared by all its chats; this is not a filesystem sandbox. The remote caller cannot enumerate/read/control other chat histories or host terminals. The machine checks this scope independently of the Cloud relay, and refuses publishing through an older runtime without scope enforcement.

## What waits on you

When Cloud lists `attention.v1` in the relay's `ready` frame, the connector tells it what waits on you, so Cloud can show it under **Needs you** and notify your phone without an open chat. A Cloud that does not list it receives nothing new (it would drop the connection on an unknown frame).

Each `attention` frame carries the complete set of pending approvals and questions (at most 200; Cloud resolves anything left out) and the chats whose turn finished or failed in the last 15 minutes (at most 20, resent in every frame; Cloud keeps the first report of each turn). The first frame of every connection is sent even when nothing waits, once every running project has answered; after that a frame goes out only when something changed, at most every 300 ms, with a `revision` that grows within the connection.

Only this leaves the computer: the ids of the approval, question, turn and chats, the project's workspace id and name, the chat title, the approval's category (`bash`, `edit`, `webfetch`, `mcp` for any MCP tool, …), whether it is a Chrome approval, whether a question takes a free-text answer, whether a chat is a scheduled run, and times. Commands, paths, URLs, approval patterns and details, question text and anything the model wrote stay on the computer; a phone reads them through the relay when it opens the chat, which is where it answers.

The connector watches only project servers that already run. It finds them through their discovery records, as the Chrome bridge does, never through the host gateway, so it never starts, replaces or keeps a server running; a stopped server runs no turn, so nothing of it can wait on you. Projects closed for remote access are left out. A server started by a different Haarness build than the connector is reported with `actionable: false`: Cloud's request would have to replace it, which Haarness refuses while a turn runs, so it is answered on the computer. A turn counts as finished only for the chat a person started, not a helper agent's, and only when nothing else starts in that chat within about two seconds, so a goal or loop that carries on is not reported; stopped turns are not reported. A chat shared with `/remote-control` reports only its own chat tree, scoped by the server exactly as Cloud's requests to it are. When the connector reconnects (an OAuth sign-in reconnects about every ten minutes), it resumes each server's events where it left off, so a turn that finishes meanwhile is still reported.

## SDKs

```typescript
const cloud = new HaarnessCloud({ baseUrl: "https://app.haarness.ai", token });
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
