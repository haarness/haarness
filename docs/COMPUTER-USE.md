# Computer use

This page covers how Haarness agents operate a Mac's own apps: switching the feature on, the `mac_*` tools, how approvals and app rules decide what may happen, what the person sees while an agent works, the native helper, the settings resource and the tests. It is written for contributors and for anyone driving the feature from a client or the API.

> **User guide:** [Computer use](https://haarness.ai/docs/desktop/computer-use).

Once switched on, an agent can photograph an app's windows, read its interface through macOS Accessibility, and send it clicks, keystrokes, scrolling and drags. That is meant for native apps with no better interface, and for checking a UI the person asked about. The prompt section steers agents elsewhere whenever a better tool exists: shell and file tools handle commands, code and settings files, the [browser tools](BROWSER.md) handle web pages, and MCP tools cover services that have them.

The feature exists only on macOS, in builds that embed the helper (all macOS release builds do), and only for the account signed in at the Mac's screen. Nothing happens until that person accepts the terms and turns it on.

## Switching it on

- **Desktop:** open **Settings → Computer use**, read the terms, tick **I have read and agree to these terms** and press **Agree and turn on**. Then grant **Accessibility** and **Screen Recording** from the **macOS permissions** rows: the first press brings up macOS's own prompts, later presses open the matching System Settings page.
- **Terminal UI:** `/mac` opens the same choices as a server-authored view.
- **CLI:** `haarness mac on` prints the terms and waits for `I agree`; `haarness mac permissions` raises the macOS prompts and opens System Settings.

macOS ties both permissions to whichever app launched the server: Haarness Desktop for the servers it starts, or the terminal app when a server was started from a shell. Settings and `haarness mac status` say which one that is. Every process descended from that app inherits the grants, including the shell commands an agent runs.

| Command | Effect |
| --- | --- |
| `haarness mac status` | Prints the on/off state, the accepted terms version, what macOS currently grants and to which app, and the app rules. `--json` gives the same status clients read. |
| `haarness mac terms` | Prints the terms. |
| `haarness mac on` / `off` | Switches computer use on (asking for the terms first when needed) or off. |
| `haarness mac withdraw` | Withdraws acceptance; the feature stays off until the terms are accepted again. |
| `haarness mac permissions` | Raises macOS's prompts and opens System Settings at the first permission still missing. |
| `haarness mac allow\|ask\|deny\|forget <bundle id>` | Adds, changes or removes the rule for an app (`com.apple.TextEdit`) or a family (`com.jetbrains.*`). |

The settings are per Mac, shared by every project, and reserved for the person: the CLI refuses to change them from inside an agent's command (`HAARNESS_SESSION_ID` present), and the settings file sits among the global configuration files that an agent's file tools may only edit with a person's approval. Turning the feature off, or withdrawing acceptance, takes effect before the agent's next step even inside a running turn, and kills the helper immediately.

## Tools

`Tool::offered` keeps the tools out of the list unless computer use is on, so a turn without it neither advertises nor executes them. The [`computer-use` prompt section](PROMPT-FILES.md) is added only on turns that offer them.

| Tool | Purpose |
| --- | --- |
| `mac_snapshot` | One app (by default the frontmost; `app` picks another, `window` narrows to one window) as a screenshot plus an outline of its Accessibility elements, each with a ref such as `@e12`. `menuBar` adds the menus. `wholeScreen` includes every app and needs approval. |
| `mac_apps` | Lists running apps, or opens, focuses or quits an app given its bundle id or display name. Opening and quitting need approval. |
| `mac_click` | Clicks a ref (preferred) or a screenshot pixel, with any button, click count and modifiers. |
| `mac_type` | Types into a ref or into the current keyboard focus; `replace` sets a field's contents in one step. |
| `mac_press` | Sends a key or shortcut in `browser_press` notation: `Enter`, `Escape`, `Meta+S`, `Control+Tab`. |
| `mac_scroll`, `mac_drag`, `mac_hover` | Scrolls over, drags between, or moves the pointer onto refs or pixels. |

Outline lines follow the browser snapshot's format, for example `@e12 [button] "Save" description="…" value="…" at=410,220 size=64x22 disabled`. Ref numbers only grow within a helper process and hold until the next snapshot; an outdated ref is reported as such instead of resolving to a different element. Positions cover only the part of an element its scroll areas and window leave visible: Accessibility keeps reporting frames for rows scrolled out of a list, and those frames lie over other controls, so such elements are left out, one line per scroll area counts them, and a ref that has scrolled away since then fails at once, with no approval card shown for it. Pixel coordinates refer to the latest screenshot, which keeps the display's aspect ratio and is at most 1456 pixels on its longer side. Unless called with `snapshot: false`, every action returns a new snapshot. Screenshots are saved beside the chat's other attachments (`.haarness-chat-attachments/mac-<id>/screen.jpg`) and reach the model as images. A project keeps the newest 200 of them for up to a week; the model is told about any picture removed since.

A single-app look draws only that app's windows, in place, over an empty background that still shows the menu bar, and strips other apps' window titles and focused elements. Password managers, macOS sign-in and authorization windows, Haarness itself and any app set to Never are left out of every screenshot, window list and outline.

## Approvals and app rules

Before an action runs, its `prepare_arguments` asks the helper what lies under the ref or pixel: the element, the app that owns it, whether it is a password field, and whether it is a native control or text and web content. Locked app groups are refused at this point, and the approval card's data goes into the prepared arguments under `__mac`: the action, the app, the element, a small picture of the spot with a ring around it, any text or keys, and a one-line `summary`. The runtime then raises a `computer` approval. A model cannot forge `__mac`: the tool schemas reject unknown keys before preparation, which overwrites the key in any case.

These approvals are explicit, like [Chrome site access](PERMISSIONS-AND-EXECUTION.md#chrome-approvals): Auto's reviewer, hooks and allow rules in configuration never answer them, though a configured or plan-agent deny still refuses. Yolo is the exception, since it approves everything: a chat in Yolo, or a `--yolo` turn, approves each of them once, also those that ask every time and those of a run nobody watches. App rules set to **Never** and the locked groups still refuse there. The plan agent is offered only `mac_snapshot` and `mac_apps`, since its ceiling refuses every action. Each pattern carries the id of the chat at the root of the conversation, so **Allow in this chat** (`always`) keeps that app approved in that chat, and in the child agents it starts, until the server restarts:

| Pattern | Raised for |
| --- | --- |
| `act:<chat>:<bundle id>` | Acting inside an app. |
| `launch:<chat>:<bundle id>` | Opening or quitting an app. Focusing an app that already runs raises nothing. |
| `screen:<chat>` | A look at every app's windows at once. |
| `each:<chat>:<bundle id>:<id>` | Approvals that never carry over: clicks landing on terminal or editor text, and anything in an app whose rule is **Ask every time**. |

For an action in an ordinary app the card also offers **Always allow** that app: it saves an **Act without asking** rule (the `app` action of the settings resource) and then allows the request once. Terminals and editors, apps set to ask every time, opening an app and the whole-screen look never offer it. The card's main line is built by the client in the person's language from the parts in `__mac` (`action`, `label`, `direction`, `keys`, `app`, `endApp`); `summary` is the English line for clients that do not know an action.

App rules apply to every project and are set in Settings → Computer use, from an approval card, or with `haarness mac allow|ask|deny`. **Act without asking** skips the approval, **Ask every time** raises one for each action, and **Never** refuses the app and blanks its windows. **Apps without a rule** decides the rest: **Ask first** (the default) or **Act without asking**.

These groups are locked regardless of rules and answers:

| Group | Allowed |
| --- | --- |
| Keychain Access, password managers, macOS sign-in and authorization windows | Nothing; invisible to agents. |
| Haarness and the macOS permission prompts | Nothing; invisible to agents. |
| The app hosting this server (the terminal of a terminal session) | Looking only, since it shows this chat and its approvals. |
| Web browsers | Looking only; page actions go through the browser tools. |
| System Settings | Looking and scrolling; settings stay with the person. |
| Terminals, code editors, script runners | Clicking and scrolling, no keyboard input and no quitting. Clicks on their text need an approval every time, because that text may contain someone's prompt; their own buttons and menus need one approval per chat whatever the rule says. |

Two kinds of input are never sent: keystrokes into a password field, and the shortcuts that sign the user out, lock the Mac or open Force Quit. The server checks the requested keys and the helper checks the keys it would actually press. Drags are checked at both ends and cannot end on the Dock. Immediately before every event the helper confirms that the app under the target, or the one holding keyboard focus, is still the approved app; when it is not, nothing is sent and the agent gets an instruction to take a new look. Long typing is checked chunk by chunk and stops as soon as focus moves to another app or onto a password field. Typing or a printable key aimed at the keyboard focus is refused when the focused element cannot be read, and while macOS reports secure keyboard input, which apps hold while a password field has focus. During that time a pointer action landing on a pop-up is refused as well: macOS opens its password suggestions (the Passwords app, a password manager) under a focused password field, inside the app's own process. A click or drag into a terminal or editor is refused while any approval of the server waits for a person: such text can show a Haarness card that takes clicks.

## While an agent works

A status strip appears along the top edge of the screen naming the app in use, with **Pause** and **Stop**. **Pause** freezes the agent: no looking and no actions until **Resume**, after which it takes a fresh look before continuing. **Stop** cancels the turn and whatever action is in progress; so does Escape pressed in any app within 45 seconds of the agent's latest screen action, as long as the strip stays up after it. Later, Escape belongs to the person's own work again, even while the turn carries on with other steps. A paused chat keeps the screen, so no other chat takes it meanwhile, and the pause ends with its turn. The strip's words follow the language of the client that most recently sent them, and are kept in `computer-use/bar.json` in the data folder so a restarted server still has them; Desktop and the terminal UI send them when they connect, since the server carries no translations. Screenshots never include the strip or the click ring.

Pointer and keyboard are shared, and the agent gives way only where input would collide: it holds off while the person types or holds down a mouse button, its own typing breaks off when the person types or clicks, and a drag in progress is put back when the person moves the pointer. Otherwise moving the pointer or scrolling interrupts nothing.

A lease lets one chat drive the screen at a time, across every Haarness server and terminal this user runs on the Mac with the same data folder (one started with another `HAARNESS_HOME` keeps a lease of its own). A child agent shares its chat's lease. A second chat that tries is told the screen is busy; the lease ends with the holding turn or 30 seconds after its latest action. It lives in `computer-use/lease.json` in the data folder and is read and written under a file lock.

## Limits

- Only the main display is captured and used.
- While the screen is locked or the display asleep, agents wait for the person to unlock or wake it; Stop still ends the wait.
- Nothing works while another user (or no user) is signed in at the console, as with a server reached over SSH.
- A published chat never gets the `mac_*` tools, in turns its viewers start or the owner's own, since its viewers see the transcript and its screenshots.
- Apps without Accessibility support, such as many games and custom canvases, offer the agent nothing but the screenshot.
- Actions happen in the foreground: the helper raises the target app and moves the pointer whenever a control cannot be pressed through Accessibility.

## Settings and API

`computer-use.json` in the global configuration folder holds the settings:

```json
{"enabled": true, "terms": {"version": "1", "acceptedAt": "2026-11-07T10:00:00Z"},
 "defaultAct": "ask", "ring": true, "apps": {"com.apple.TextEdit": "allow", "com.jetbrains.*": "ask"}}
```

The feature counts as on only with `enabled` true and `terms.version` equal to the current version, so publishing new terms switches it off until they are accepted. Clients talk to the `computer-use` management resource (`POST /v1/manage/computer-use`, advertised as `computer-use.v1` in `/v1/info`); its view carries the status in `data`. Actions: `get`; `words`, which stores the strip's wording without starting the helper; `accept` with `version`; `enable` with `enabled`; `withdraw`; `app` with `bundle` and `rule` (`allow`, `ask`, `deny` or `clear`); `configure` with `defaultAct` and `ring`; `permissions`; and `open` with `pane` (`accessibility` or `screen-recording`). Any action may include `bar`, the strip's words in the client's language. The status reports `supported`, `console`, `enabled`, `terms` (including the text to render), `permissions`, `screen` (locked, asleep), `host` (the app macOS grants to), `paused`, `active` and `settings`.

Only a person gives agents more reach. A change that would (`accept`, turning on, `defaultAct` allow, hiding the click ring, moving an app's rule toward allow) is refused when it comes from a program Haarness started, such as an agent's shell command, a hook or a Haarness terminal: the server finds the local process behind the request and refuses when a `haarness` process is among its ancestors, and `haarness mac` checks its own ancestry the same way. Switching off, withdrawing and tightening always work. The settings file itself is guarded like Haarness's sign-in files: a model-run read or edit of it, or a shell command naming it, asks a person every time, in every approval mode. A program that detaches itself from Haarness on purpose escapes the ancestry check; under Yolo an agent's commands also run with the macOS permissions of the app that launched Haarness, so Yolo deserves the same care with computer use on as without it.

## The native helper

The Swift package in `crates/haarness-computer/macos` has two targets: `MacKit`, unit-tested logic with no window-server dependency (geometry, key combinations, outline lines, the strip's wording), and `MacHelper`, the executable. The helper runs as an accessory app with no Dock icon so it can draw the strip and the click ring. The protocol is newline-delimited JSON on stdin and stdout: a request `{"id": 3, "op": "snapshot", …}` is answered by `{"id": 3, "ok": true, "data": {…}}` or `{"id": 3, "ok": false, "code": "…", "message": "…"}`, and unsolicited lines with an `event` field (`escape`, `pause`, `resume`, `stop`) report the person's input. `haarness-mac --version` prints the protocol and build.

The crate's build script compiles the package with `xcrun swiftc` for the architecture being built (macOS 12 and later), and the Rust binary embeds the result; other platforms embed nothing, and `HAARNESS_SKIP_MAC_HELPER=1` builds without it. On first use the server writes the helper into the cache folder (`mac-helper/<version>/<hash>/haarness-mac`) and verifies its hash before every launch. Requests are serialized; Stop or a timeout kills the process, and it exits five minutes after its latest request, or ten seconds when it was only started to read permissions while the feature is off. `HAARNESS_MAC_HELPER`, an absolute path, substitutes another helper such as a local development build.

## Tests

```sh
cargo test -p haarness-computer                            # policy, settings, lease, tools, and the runtime against a stand-in helper
swift test --package-path crates/haarness-computer/macos   # the helper's own logic
cargo test -p haarness-tui --lib -- computer               # the terminal approval card
HAARNESS_COMPUTER_LIVE=1 python3 scripts/test-computer-use-live.py   # the real screen on this Mac
```

`crates/haarness-computer/tests/runtime.rs` runs the real runtime with a scripted model and `tests/fake-helper.py`, a stand-in that replies the way the helper would without touching the screen. It covers approvals and their scope, app rules, the locked groups, terminals, the lease, pausing, Escape, and switching the feature off mid-turn. `scripts/test-computer-use-live.py` compiles a small lab app (`scripts/computer-use-lab.swift`) and exercises every action against it on the real screen through the embedded helper, including Escape interrupting a long typing turn. It needs Accessibility and Screen Recording for the terminal app it runs in and an unlocked screen, and it takes over the pointer while it runs.
