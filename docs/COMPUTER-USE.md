# Computer use

This page covers how Haarness agents operate a Mac's own apps: switching the feature on, the `mac_*` tools, how approvals and app rules decide what may happen, how the agent works beside the person, what the person sees while an agent works, the native helper, the settings resource and the tests. It is written for contributors and for anyone driving the feature from a client or the API.

> **User guide:** [Computer use](https://haarness.ai/docs/desktop/computer-use).

Once switched on, an agent can photograph an app's windows, read its interface through macOS Accessibility, and send it clicks, keystrokes, scrolling and drags. That is meant for native apps with no better interface, and for checking a UI the person asked about. The prompt section steers agents elsewhere whenever a better tool exists: shell and file tools handle commands, code and settings files, the [browser tools](BROWSER.md) handle web pages, and MCP tools cover services that have them.

The feature exists only on macOS, in builds that embed the helper (all macOS release builds do), and only for the account signed in at the Mac's screen. Nothing happens until that person accepts the terms and turns it on.

## Switching it on

- **Desktop:** open **Settings → Computer use**, read the terms, tick **I have read and agree to these terms** and press **Agree and turn on**. When the person agreed to an earlier version of the terms, one quiet line beside the agreement says so, and agreeing again is the same single step. Then grant **Accessibility** and **Screen Recording** from the **macOS permissions** rows: the first press brings up macOS's own prompts, later presses open the matching System Settings page.
- **Terminal UI:** `/mac` opens the same choices as a server-authored view. It is listed on macOS only.
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
| `mac_snapshot` | One app (by default the frontmost; `app` picks another, `window` narrows to one window) as a screenshot plus an outline of its Accessibility elements, each with a ref such as `@e12`. `menuBar` adds the menus, `find` keeps only elements matching a word or phrase (plus the elements around them), and `zoom` (`x`, `y`, `width`, `height` in the latest screenshot's pixels) shows that area at full resolution while positions stay in the screenshot's frame; the result names the area the helper really showed, and claims no close-up when the helper showed none. `wholeScreen` includes every app and needs approval. |
| `mac_apps` | Lists running apps, or opens, focuses or quits an app given its bundle id or display name. `launch` opens the app behind the person's window; `activate` brings it to the front. Opening and quitting need approval. |
| `mac_click` | Clicks a ref (preferred) or a screenshot pixel, with any button, click count and modifiers. |
| `mac_type` | Types into a ref or into the current keyboard focus; `replace` sets a field's contents in one step. |
| `mac_press` | Sends a key or shortcut in `browser_press` notation: `Enter`, `Escape`, `Meta+S`, `Control+Tab`. |
| `mac_scroll`, `mac_drag`, `mac_hover` | Scrolls over, drags between, or moves the pointer onto refs or pixels. |

Outline lines follow the browser snapshot's format, for example `@e12 [button] "Save" description="…" value="…" at=410,220 size=64x22 disabled`. Ref numbers only grow within a helper process and hold until the next snapshot; an outdated ref is reported as such instead of resolving to a different element. Positions cover only the part of an element its scroll areas and window leave visible: Accessibility keeps reporting frames for rows scrolled out of a list, and those frames lie over other controls, so such elements are left out, one line per scroll area counts them, and a ref that has scrolled away since then fails at once, with no approval card shown for it. Pixel coordinates refer to the latest screenshot, which keeps the display's aspect ratio and is at most 1456 pixels on its longer side. Unless called with `snapshot: false`, every action returns a new snapshot, of the app it acted on when the helper names that app. Screenshots are saved beside the chat's other attachments (`.haarness-chat-attachments/mac-<id>/screen.jpg`) and reach the model as images. A project keeps the newest 200 of them for up to a week; the model is told about any picture removed since.

A single-app look draws only that app's windows, in place, over an empty background that still shows the menu bar, and strips other apps' window titles and focused elements. Password managers, macOS sign-in and authorization windows, Haarness itself and any app set to Never are left out of every screenshot, window list and outline.

## Approvals and app rules

Before an action runs, its `prepare_arguments` asks the helper what lies under the ref or pixel: the element, the app that owns it, whether it is a password field, and whether it is a native control or text and web content. Locked app groups are refused at this point, and the approval card's data goes into the prepared arguments under `__mac`: the action, the app, the element, a small picture of the spot with a ring around it, any text or keys, and a one-line `summary`. The runtime then raises a `computer` approval. A model cannot forge `__mac`: the tool schemas reject unknown keys before preparation, which overwrites the key in any case.

These approvals are explicit, like [Chrome site access](PERMISSIONS-AND-EXECUTION.md#chrome-approvals): hooks and allow rules in configuration never answer them, though a configured or plan-agent deny still refuses, and a schedule's run is refused instead of waiting: it uses only apps set to **Act without asking**, also on a server started with `serve --yolo`. In a chat in Auto with its reviewer on, [Auto's reviewer](AUTO-APPROVALS.md#autos-reviewer) answers `act`, `launch` and `screen` requests as it answers other requests (`Tool::reviewer_may_answer`): it approves the request once, leaves it to the person with its note on the card, or blocks it. It reads what the card says in words (the app, the element, the window, the part of it around the element, the menu item a ⌘ shortcut would choose, System Settings' pane, the text, the keys, a drop's app) and never the picture of the spot; its prompt says those words come from the app and are untrusted. These stay with the person, unreviewed: anything in an app set to **Ask every time**; anything in a terminal, script runner, REPL, notebook, launcher or editor that runs shell commands (a click on Paste or Run there runs code the reviewer never sees); anything in a code editor, whose menus and command palette run selected text, the open file or a task; anything in System Settings when the helper cannot name the open pane; and anything in a published chat, whose viewers could set its mode. With the reviewer off, Auto leaves every computer use request to the person. Yolo approves everything: a chat in Yolo, or a `--yolo` turn (`haarness run --yolo`, ACP, a terminal UI started with `--yolo`), approves each of them once, also those that ask every time. A schedule's run approves its own requests too, but not these: nobody is there to press Stop, so it acts only in apps set to **Act without asking**, also on a `serve --yolo` server. App rules set to **Never** and the locked groups still refuse there. The plan agent is offered only `mac_snapshot` and `mac_apps`, since its ceiling refuses every action. Each pattern carries the id of the chat at the root of the conversation, so **Allow in this chat** (`always`) keeps that app approved in that chat, and in the child agents it starts, until the server restarts:

| Pattern | Raised for |
| --- | --- |
| `act:<chat>:<bundle id>` | Acting inside an app. |
| `launch:<chat>:<bundle id>` | Opening or quitting an app. Focusing an app that already runs raises nothing. |
| `screen:<chat>` | A look at every app's windows at once. |
| `each:<chat>:<bundle id>:<id>` | Approvals that never carry over and only a person answers: anything in an app whose rule is **Ask every time**. |

For an action in an app the card also offers **Always allow** that app: it saves an **Act without asking** rule (the `app` action of the settings resource) and then allows the request once. Apps set to ask every time, opening an app and the whole-screen look never offer it. Allowing a terminal for good also lets agents re-run the commands it keeps, such as those listed in Terminal's New Command sheet, with clicks alone. A drag from one app into another asks only about the apps not yet allowed, and the card names the one it asks about, which is the app **Always allow** saves (often the drop's, not the pickup's). When it asks about both, the card offers no **Always allow**, and **Allow in this chat** covers both apps. The card's main line is built by the client in the person's language from the parts in `__mac` (`action`, `label`, `direction`, `keys`, `app`, `endApp`), with the marks and isolates that set text direction taken out of each part, since an app's label could otherwise turn the rest of the line around; `summary` is the English line for clients that do not know an action.

App rules apply to every project and are set in Settings → Computer use, from an approval card, or with `haarness mac allow|ask|deny`. **Act without asking** skips the approval, **Ask every time** raises one for each action, and **Never** refuses the app and blanks its windows. **Apps without a rule** decides the rest: **Ask first** (the default) or **Act without asking**.

These groups are locked regardless of rules and answers:

| Group | Allowed |
| --- | --- |
| Keychain Access, password managers, macOS sign-in and authorization windows | Nothing; invisible to agents. |
| Haarness and the macOS permission prompts | Nothing; invisible to agents. |
| The app hosting this server (the terminal of a terminal session) | Looking only, since it shows this chat and its approvals. |
| Terminals, script runners and automation editors (Script Editor, Automator, Shortcuts, Script Debugger, Keyboard Maestro, Hammerspoon), REPLs and notebooks (IDLE, JupyterLab Desktop), launchers with a command line (Alfred, Raycast) and editors that run shell commands (MacVim, VimR, Neovide, Emacs) | Everything but keyboard input: no typing, keys, modifier clicks, drops or Paste (`paste` from the helper, which also refuses a click that lands on Paste when the request carries `refusePaste`). What is typed there runs outside Haarness's approvals, including `haarness mac allow`, which a terminal of the person's own is free to run; commands go through the shell tool. The same holds for a terminal inside another app, such as an editor's terminal panel, when the helper reports the element as one (`terminal`). |
| System Settings' Privacy & Security pane | Looking and scrolling, when the helper names the open pane (`pane`) or the element belongs to the pane's own process; it decides what apps and agents may do on this Mac. When the helper cannot tell the pane, System Settings follows the app rules, and a person answers its requests even in Auto. |

Every other app follows the app rules and approvals, browsers, the rest of System Settings and code editors included: the browser tools stay the better way to work on a web page, and a settings change is the kind of action Auto's reviewer weighs as risky.

Two kinds of input are never sent: keystrokes into a password field, and the shortcuts that sign the user out, lock the Mac or open Force Quit. The server checks the requested keys and the helper checks the keys it would actually press. Drags are checked at both ends and cannot end on the Dock. Immediately before every event the helper confirms that the app under the target, or the one holding keyboard focus, is still the approved app; when it is not, nothing is sent and the agent gets an instruction to take a new look. Long typing is checked chunk by chunk and stops as soon as focus moves to another app or onto a password field. Typing or a printable key aimed at the keyboard focus is refused when the focused element cannot be read, and while macOS reports secure keyboard input, which apps hold while a password field has focus. During that time a pointer action landing on a pop-up is refused as well: macOS opens its password suggestions (the Passwords app, a password manager) under a focused password field, inside the app's own process. While any approval of the server waits for a person, clicks, drags, typing and key presses in terminals and code editors are refused: such a window (an editor's built-in terminal too) can show Haarness's terminal UI, whose card takes keys and clicks.

## Working beside the agent

Agents act in the app they last looked at, wherever it sits on screen, and the person keeps their own. A single-app look draws a covered app's windows in place; clicks, scrolling, typing and key presses then reach that app without raising it, moving the pointer or taking the keyboard from the person's front app. Typing and keys without a ref go to the looked-at app's own focused element, and a pixel inside its windows belongs to it even where another window covers that spot. A whole-screen look forgets the looked-at app, and actions without one go to the app in front, as before. Drags and hovering need the real pointer, so they bring their app to the front, and so does any action the helper cannot deliver from behind (it cannot prove which window takes the input, or this macOS lacks what it needs); `HAARNESS_MAC_FOREGROUND=1` keeps every action in front for diagnosis. `mac_apps` `launch` starts an app behind the person's window; `activate` brings it forward.

Each result says what happened, for the model to read:

- that the action went in behind the person's window, or why its app came to the front;
- whether the app showed the change; when it did not, the result asks for a fresh look first, and after a click or scroll from behind it suggests `mac_apps activate`;
- the windows, sheets, dialogs and menus the action opened or closed, titled for the app acted on and by app name only for others.

The fresh look that follows is at the app acted on, not the one in front, and a look says when its app is behind the person's window, or shows none of its windows in this Space (each one minimized, or placed on a different desktop). The helper reports these in its replies (`background`, `via`, `confirmed`, `target`, `opened`, `closed` and `fallback` for an action; `behind` and `offscreen` for a look). Each is optional: a helper that sends none gives the results of before, with the look at the app in front.

## While an agent works

A status strip appears along the top edge of the screen naming the app in use, with **Pause** and **Stop**. **Pause** freezes the agent: no looking and no actions until **Resume**, after which it takes a fresh look before continuing. While the pause lasts, the chat holding the screen (and any agent it started) says so in place of its working line whenever its turn waits on the screen, that is while one of its `mac_*` steps has no result yet, with nothing to answer; a turn busy with something else, such as a command or its reply, keeps its own line: in Desktop the activity line under the transcript stops moving and reads **Paused**, followed by **Resume**; in the terminal UI the step waiting for the screen reads `‖ mac_… paused · /mac resume` and the status band `paused`. That **Resume**, `/mac resume`, or a click on it on the step does what the strip's button does. A Resume request from a program Haarness started is refused ([Settings and API](#settings-and-api) says how that is told and where it stops). A helper that quits during a pause, killed or crashed, takes the strip with it but not the pause: the agent waits until the person resumes it in the chat, and the next helper starts unpaused. **Stop** cancels the turn and whatever action is in progress, also one whose approval is still open; so does Escape pressed within 45 seconds of the agent's latest screen action, as long as the strip stays up after it, while the agent acts in the front app: the helper reports Escape only then. While the agent works behind the person's window, Escape belongs to the app the person is using, and **Stop** is the way to stop the agent. Later, Escape belongs to the person's own work again, even while the turn carries on with other steps. A paused chat keeps the screen, so no other chat takes it meanwhile, and the pause ends with its turn. A pause pressed before the agent first used the screen (while an approval card waited, say) is told to the chat as soon as the agent waits for it. The strip's words follow the language of the client that most recently sent them, and are kept in `computer-use/bar.json` in the data folder so a restarted server still has them; Desktop and the terminal UI send them when they connect, since the server carries no translations. Screenshots never include the strip or the click ring.

Pointer and keyboard are shared, and the agent gives way only where input would collide: it holds off while the person types or holds down a mouse button, its own typing breaks off when the person types or clicks, and a drag in progress is put back when the person moves the pointer. Otherwise moving the pointer or scrolling interrupts nothing.

A lease lets one chat drive the screen at a time, across every Haarness server and terminal this user runs on the Mac with the same data folder (one started with another `HAARNESS_HOME` keeps a lease of its own). A child agent shares its chat's lease. A second chat that tries is told the screen is busy; the lease ends with the holding turn or 30 seconds after its latest action. It lives in `computer-use/lease.json` in the data folder and is read and written under a file lock.

## Limits

- Only the main display is captured and used.
- While the screen is locked or the display asleep, agents wait for the person to unlock or wake it; Stop still ends the wait.
- Nothing works while another user (or no user) is signed in at the console, as with a server reached over SSH.
- A published chat never gets the `mac_*` tools, in turns its viewers start or the owner's own, since its viewers see the transcript and its screenshots.
- Apps without Accessibility support, such as many games and custom canvases, offer the agent nothing but the screenshot.
- Drags and hovering bring their app to the front and move the pointer, and so does any action the helper cannot deliver from behind; the result says so.

## Settings and API

`computer-use.json` in the global configuration folder holds the settings:

```json
{"enabled": true, "terms": {"version": "2", "acceptedAt": "2026-11-10T10:00:00Z"},
 "defaultAct": "ask", "ring": true, "apps": {"com.apple.TextEdit": "allow", "com.jetbrains.*": "ask"}}
```

The feature counts as on only with `enabled` true and `terms.version` equal to the current version, so publishing new terms switches it off until they are accepted. Version 2 says that Auto's reviewer may answer computer use requests in a chat set to Auto; a person who agreed to version 1 is shown the terms again, with a line saying they agreed to an earlier version (the status's `terms.changed`), and an agent whose call finds computer use off for this reason is told so, to tell the person. Clients talk to the `computer-use` management resource (`POST /v1/manage/computer-use`, advertised as `computer-use.v1` in `/v1/info`); its view carries the status in `data`. Actions: `get`; `words`, which stores the strip's wording without starting the helper and answers `supported`, `paused` and `pausedChat`, so a client that connects during a pause shows it at once; `resume`, which lets a paused agent go on as the strip's **Resume** does, telling the strip first, and changes nothing when no agent is paused (its answer adds `resumed`, whether an agent was paused, which clients word their reply from); `accept` with `version`; `enable` with `enabled`; `withdraw`; `app` with `bundle` and `rule` (`allow`, `ask`, `deny` or `clear`); `configure` with `defaultAct` and `ring`; `permissions`; and `open` with `pane` (`accessibility` or `screen-recording`). Any action may include `bar`, the strip's words in the client's language. The status reports `supported`, `console`, `enabled`, `terms` (including the text to render, and `changed` when the person agreed to earlier terms), `permissions`, `screen` (locked, asleep), `host` (the app macOS grants to), `paused`, `pausedChat` (the chat at the root of the paused conversation, while the pause lasts), `active` and `settings`.

When the person pauses, the server publishes `computer_use.updated` with `{"paused": true}` for the paused chat, and the same event with `{"paused": false}` when the pause ends, however it ends: **Resume** on the strip or in a client, **Stop**, Escape, switching off, or the end of the turn.

Only a person gives agents more reach. A change that would (`accept`, turning on, `defaultAct` allow, hiding the click ring, moving an app's rule toward allow, or `resume` while an agent is paused) is refused when it comes from a program Haarness started, such as an agent's shell command, a hook or a Haarness terminal. The server finds the local process behind the request and refuses when a `haarness` process is among its ancestors, or when that process, or one above it, runs in a process group Haarness started for a command; `haarness mac` checks itself the same way, and also refuses while `HAARNESS_SESSION_ID` is set. A command's shell starts without job control, so a program the command puts in the background (`… &`, `nohup`, `( … & )`) stays in the command's group, also once the system hands it to launchd and so takes it out of the tree. Such a program is refused while the command runs, and the server stops the group, with whatever is still in it, when the command ends; for the moment between the two, only the server that ran the command still refuses it. Switching off, withdrawing and tightening always work, from anywhere.

What the check does not cover, which is why Yolo deserves the same care with computer use on as without it:

- The server's token proves nothing about who asks: any program running as this user can read it, so only the process behind the request tells.
- A program that moves to a process group or session of its own is recognised only while it is still below the command's shell in the tree, and the server's stop at the end of the command does not reach it. Shell job control moves a program there (`set -m`, under which each job, `( … & )` included, gets a group of its own), and so do `setsid` and programs that detach themselves the way daemons do. Once its parent has exited, such a program can widen computer use and end a pause, and it can outlive its command.
- A program that launchd or another app starts for a command (`launchctl`, `at`, `open`) is not recognised either.
- An agent's commands run with the macOS permissions of the app that launched Haarness. With Accessibility granted, a command could operate the person's own windows, Settings → Computer use and the strip included.
- The settings file is guarded by its name: a model-run read or edit of it, or a shell command naming it, asks a person every time, in every approval mode, like Haarness's sign-in files. A program that writes it without naming it on its command line is not stopped.

## The native helper

The Swift package in `crates/haarness-computer/macos` has two targets: `MacKit`, unit-tested logic with no window-server dependency (geometry, key combinations, outline lines, the strip's wording), and `MacHelper`, the executable. The helper runs as an accessory app with no Dock icon so it can draw the strip and the click ring. The protocol is newline-delimited JSON on stdin and stdout: a request `{"id": 3, "op": "snapshot", …}` is answered by `{"id": 3, "ok": true, "data": {…}}` or `{"id": 3, "ok": false, "code": "…", "message": "…"}`, and unsolicited lines with an `event` field (`escape`, `pause`, `resume`, `stop`) report the person's input. `haarness-mac --version` prints the protocol and build.

The crate's build script compiles the package with `xcrun swiftc` for the architecture being built (macOS 12 and later), and the Rust binary embeds the result; other platforms embed nothing, and `HAARNESS_SKIP_MAC_HELPER=1` builds without it. On first use the server writes the helper into the cache folder (`mac-helper/<version>/<hash>/haarness-mac`) and verifies its hash before every launch. Requests are serialized; Stop or a timeout kills the process, and it exits five minutes after its latest request, or ten seconds when it was only started to read permissions while the feature is off. `HAARNESS_MAC_HELPER`, an absolute path, substitutes another helper such as a local development build.

## Tests

```sh
cargo test -p haarness-computer                            # policy, settings, lease, tools, and the runtime against a stand-in helper
swift test --package-path crates/haarness-computer/macos   # the helper's own logic
cargo test -p haarness-tui --lib -- computer paus          # the terminal approval card and the paused step
npm --prefix apps/desktop test                             # the Desktop card and the paused line's rules
(cd apps/desktop && npx electron-vite build && npx playwright test e2e/computer-use.spec.ts)   # Settings, cards and the paused line
HAARNESS_COMPUTER_LIVE=1 python3 scripts/test-computer-use-live.py   # the real screen on this Mac
```

`crates/haarness-computer/tests/runtime.rs` runs the real runtime with a scripted model and `tests/fake-helper.py`, a stand-in that replies the way the helper would without touching the screen. It covers approvals and their scope, Auto's reviewer answering them, in turns and in workflows (and not answering an app set to ask every time, a terminal, a published chat, a schedule's run, nor with the reviewer off), what the reviewer reads, hooks and configured allow rules never answering, app rules, the locked groups, typing into terminals and terminal panels, the Privacy & Security pane, input into a terminal or editor while an approval waits, what results say about an action behind the person's window, `find` and `zoom`, the lease, pausing (what clients hear, **Resume** from a client, **Stop** on a paused strip, a helper quitting during a pause), Escape, and switching the feature off mid-turn. Desktop's spec runs a real server with the same stand-in (`HAARNESS_MAC_HELPER`) to pause a chat and resume it from its activity line. `scripts/test-computer-use-live.py` compiles a small lab app (`scripts/computer-use-lab.swift`) and exercises every action against it on the real screen through the embedded helper, including Escape interrupting a long typing turn. It needs Accessibility and Screen Recording for the terminal app it runs in and an unlocked screen, and it takes over the pointer while it runs.
