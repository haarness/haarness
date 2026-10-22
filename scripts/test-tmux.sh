#!/bin/sh
set -eu

binary=${HAARNESS_BIN:-"$PWD/target/debug/haarness"}
session="haarness-e2e-$$"
# The gate checks what a local truecolor UTF-8 terminal renders, so it sets
# that environment instead of inheriting the caller's: without COLORTERM
# OpenTUI reports no RGB support, and in an SSH session it skips terminal
# detection, so the warm page background never renders. Panes get it
# explicitly because a running tmux server keeps its own environment.
export LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 COLORTERM=truecolor
unset SSH_TTY SSH_CONNECTION SSH_CLIENT
pane_env="env -u SSH_TTY -u SSH_CONNECTION -u SSH_CLIENT COLORTERM=truecolor LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8"
test_root="$(mktemp -d)"
clipboard_saved=0
managed_workspace_path=''
if command -v pbpaste >/dev/null 2>&1 && command -v pbcopy >/dev/null 2>&1; then
  pbpaste > "$test_root/original-clipboard" 2>/dev/null || true
  clipboard_saved=1
fi
cleanup() {
  tmux kill-session -t "$session" 2>/dev/null || true
  # The TUI is a client of a persistent execution server. Stop both isolated
  # servers explicitly before deleting their data directories so this gate
  # never leaves detached test daemons behind.
  if [ -x "$binary" ] && [ -d "$test_root/ws" ]; then
    if [ -n "$managed_workspace_path" ] && [ -d "$managed_workspace_path" ]; then
      env HAARNESS_DATA_DIR="$test_root" \
        "$binary" --workspace "$managed_workspace_path" serve --stop \
        >/dev/null 2>&1 || true
    fi
    env HAARNESS_DATA_DIR="$test_root" \
      "$binary" --workspace "$test_root/ws" serve --stop \
      >/dev/null 2>&1 || true
    env HAARNESS_DATA_DIR="$test_root/unresponsive-data" \
      "$binary" --workspace "$test_root/unresponsive-ws" serve --stop \
      >/dev/null 2>&1 || true
    env HAARNESS_DATA_DIR="$test_root/suspend-data" \
      "$binary" --workspace "$test_root/ws" serve --stop \
      >/dev/null 2>&1 || true
  fi
  if [ "$clipboard_saved" -eq 1 ]; then
    pbcopy < "$test_root/original-clipboard" 2>/dev/null || true
  fi
  rm -rf "$test_root"
}
trap cleanup EXIT INT TERM

# Convert a captured UTF-8 row plus a literal label into a one-based SGR
# mouse column. awk's index() reports byte offsets on macOS, which points past
# header controls whenever the breadcrumb contains glyphs such as ›, …, or ↑.
cell_column() (
  cell_line=$1
  cell_label=$2
  cell_prefix=${cell_line%%"$cell_label"*}
  [ "$cell_prefix" != "$cell_line" ] || return 1
  # Count characters, not bytes: dash (Debian's sh) gives ${#var} in bytes,
  # and the rows hold arrows. Select the label's second cell so the click is
  # deliberately inside it.
  printf '%s\n' "$(( $(printf '%s' "$cell_prefix" | wc -m) + 2 ))"
)

workspace_runtime_path() (
  runtime_data=$1
  runtime_workspace=$(CDPATH= cd -- "$2" && pwd -P)
  runtime_id=$(jq -er --arg path "$runtime_workspace" \
    'first(.workspaces[] | select(.path == $path) | .id)' \
    "$runtime_data/data/workspaces.json")
  printf '%s/cache/workspaces/%s/runtime.json\n' "$runtime_data" "$runtime_id"
)

if [ -z "${HAARNESS_BIN:-}" ]; then
  cargo build -q -p haarness-cli
fi

# An explicit stop must recover a daemon that still owns the workspace lease
# after its listener becomes unresponsive. Freeze an isolated real daemon to
# exercise the TERM/KILL fallback and stale-discovery cleanup end to end.
mkdir -p "$test_root/unresponsive-ws"
env HAARNESS_DATA_DIR="$test_root/unresponsive-data" \
  "$binary" --workspace "$test_root/unresponsive-ws" serve --daemon >/dev/null
unresponsive_runtime=$(workspace_runtime_path \
  "$test_root/unresponsive-data" "$test_root/unresponsive-ws")
unresponsive_pid="$(jq -r '.pid' "$unresponsive_runtime")"
kill -STOP "$unresponsive_pid"
env HAARNESS_DATA_DIR="$test_root/unresponsive-data" \
  "$binary" --workspace "$test_root/unresponsive-ws" serve --stop >/dev/null
if kill -0 "$unresponsive_pid" 2>/dev/null || [ -e "$unresponsive_runtime" ]; then
  echo "Explicit stop did not recover the unresponsive workspace daemon" >&2
  exit 1
fi

mkdir -p "$test_root/config/skills/demo"
printf -- '---\ndescription: Demo e2e skill\n---\nSkill payload for the tmux gate.\n' > "$test_root/config/skills/demo/SKILL.md"
mkdir -p "$test_root/ws"
# Attachments are judged by their bytes, so image fixtures must be real images.
png_fixture() { printf '\211\120\116\107\015\012\032\012\000\000\000\015\111\110\104\122\000\000\000\001\000\000\000\001\010\006\000\000\000\037\025\304\211\000\000\000\013\111\104\101\124\170\234\143\140\000\002\000\000\005\000\001\172\136\253\077\000\000\000\000\111\105\116\104\256\102\140\202' > "$1"; }
png_fixture "$test_root/ws/dropped image.png"
printf -- '%%PDF-1.4\n1 0 obj<<>>endobj\n%%%%EOF\n' > "$test_root/ws/dropped report.pdf"
mkdir -p "$test_root/external-list"
printf -- 'approved external fixture\n' > "$test_root/external-list/visible.txt"
mkdir -p "$test_root/ws/.haarness/workflows"
printf -- 'export const meta = { name: "tmux-demo", phases: ["verify"] }\nphase("verify")\nlog("tmux workflow fixture")\nreturn { value: args.value }\n' > "$test_root/ws/.haarness/workflows/tmux-demo.js"
mkdir -p "$test_root/00-dot-nav"
png_fixture "$test_root/00-dot-nav/parent image.png"
mkdir -p "$test_root/config/themes"
printf -- '[dark]\nbg = "#102030"\n' > "$test_root/config/themes/e2e.toml"
# Parity defaults auto-run bash/writes; the e2e approval stanzas exercise the
# prompt flow, so tighten via config exactly as a cautious user would.
printf -- '[permissions]\nbash = "ask"\nwrite = "ask"\n' > "$test_root/config/config.toml"
# Haarness-owned command fixture; Claude Code project settings are never imported.
mkdir -p "$test_root/ws/.haarness/commands"
printf -- '---\ndescription: Greet someone\nargument-hint: "[name]"\n---\nSay hello to $ARGUMENTS with enthusiasm.\n' > "$test_root/ws/.haarness/commands/hello.md"
printf -- '{"permission":{"bash":{"rm *":"deny"}}}\n' > "$test_root/ws/haarness.json"
# A real Claude Code-format marketplace plugin. Its command must be
# namespaced and visible in the TUI after installation.
mkdir -p "$test_root/market/.claude-plugin"
mkdir -p "$test_root/market/demo/.claude-plugin"
mkdir -p "$test_root/market/demo/commands"
printf -- '{"name":"fixture-market","plugins":[{"name":"demo","source":"./demo","description":"Demo Claude plugin"}]}\n' > "$test_root/market/.claude-plugin/marketplace.json"
printf -- '{"name":"demo","description":"Demo Claude plugin"}\n' > "$test_root/market/demo/.claude-plugin/plugin.json"
printf -- '---\ndescription: Ship from plugin\n---\nPlugin shipped $ARGUMENTS.\n' > "$test_root/market/demo/commands/shipit.md"
env HAARNESS_DATA_DIR="$test_root" "$binary" plugin marketplace "$test_root/market" >/dev/null
env HAARNESS_DATA_DIR="$test_root" "$binary" plugin install demo@fixture-market >/dev/null

# Real Git review fixture: keep a default branch plus one committed feature
# change so `/diff git`, `/diff branch`, and `/diff last` exercise distinct
# sources through the running server and terminal UI.
git -C "$test_root/ws" init -q
git -C "$test_root/ws" config user.name "Haarness E2E"
git -C "$test_root/ws" config user.email "haarness-e2e@example.invalid"
git -C "$test_root/ws" add .
git -C "$test_root/ws" commit -qm "fixture baseline"
git -C "$test_root/ws" branch -M main
git -C "$test_root/ws" checkout -qb e2e-review
printf -- 'committed branch review\n' > "$test_root/ws/branch-review.txt"
git -C "$test_root/ws" add branch-review.txt
git -C "$test_root/ws" commit -qm "branch review fixture"
# Worktree creation below deliberately selects a real non-current base from
# the discovered-ref picker. Capture its exact commit now so the resulting
# worktree can be checked independently of its generated branch name.
workspace_base_ref=main
workspace_base_commit="$(git -C "$test_root/ws" rev-parse "$workspace_base_ref^{commit}")"
if [ "$(git -C "$test_root/ws" branch --show-current)" = "$workspace_base_ref" ]; then
  echo "Workspace base-ref fixture must not be the current branch" >&2
  exit 1
fi

tmux new-session -d -x 100 -y 30 -s "$session" "$pane_env HAARNESS_DATA_DIR='$test_root' HAARNESS_TEST_PROVIDER=1 HAARNESS_DEFAULT_PROVIDER=echo HAARNESS_DEFAULT_MODEL=echo-1 '$binary' --workspace '$test_root/ws'"

attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "h a a r n e s s" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "h a a r n e s s"; then
  echo "$pane" >&2
  echo "TUI did not render the Haarness header" >&2
  exit 1
fi
# Home screen: the masthead settles onto the composer and carries the key
# legend without turning the launch surface into a dashboard.
if ! echo "$pane" | grep -q "/ commands" || echo "$pane" | grep -q "ctrl+o tasks"; then
  echo "$pane" >&2
  echo "Home screen key legend did not render" >&2
  exit 1
fi
if ! echo "$pane" | grep -q "h a a r n e s s" || ! echo "$pane" | grep -Eq "v[0-9]+\.[0-9]+\.[0-9]+"; then
  echo "$pane" >&2
  echo "Home screen identity line did not render" >&2
  exit 1
fi
styled_pane="$(tmux capture-pane -p -e -t "$session":0.0 2>/dev/null || true)"
# The pristine home intentionally has no raised header/dashboard; it should
# still paint the configured page background through OpenTUI.
if ! echo "$styled_pane" | grep -q "48;2;21;20;19m"; then
  echo "$styled_pane" >&2
  echo "OpenTUI did not render the expected warm background" >&2
  exit 1
fi
if echo "$styled_pane" | grep -q "48;2;0;0;255m"; then
  echo "$styled_pane" >&2
  echo "OpenTUI regressed to the invalid blue background" >&2
  exit 1
fi

# Agent cycling follows Haarness: Tab advances and the terminal's real
# back-tab sequence (Shift+Tab) returns to the previous agent.
tmux send-keys -t "$session":0.0 Tab
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq "plan[[:space:]]+echo/(echo-1|Echo 1)" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
if ! echo "$pane" | grep -Eq "plan[[:space:]]+echo/(echo-1|Echo 1)"; then
  echo "$pane" >&2
  echo "Tab did not advance to the next agent" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 BTab
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq "build[[:space:]]+echo/(echo-1|Echo 1)" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
if ! echo "$pane" | grep -Eq "build[[:space:]]+echo/(echo-1|Echo 1)"; then
  echo "$pane" >&2
  echo "Shift+Tab did not return to the previous agent" >&2
  exit 1
fi

# Ctrl+Z parity needs a real interactive shell with job control. A directly
# launched, orphaned process group can legally ignore SIGTSTP on macOS and
# would turn this into a false result, so prove stop, fg, redraw, input, and
# final shell restoration in a dedicated tmux window.
mkdir -p "$test_root/suspend-data"
tmux new-window -d -t "$session" -n suspend
suspend_target="$session:suspend.0"
suspend_shell_pid="$(tmux display-message -p -t "$suspend_target" '#{pane_pid}')"
suspend_launch="$pane_env HAARNESS_DATA_DIR='$test_root/suspend-data' HAARNESS_TEST_PROVIDER=1 HAARNESS_DEFAULT_PROVIDER=echo HAARNESS_DEFAULT_MODEL=echo-1 '$binary' --workspace '$test_root/ws'"
tmux send-keys -l -t "$suspend_target" "$suspend_launch"
tmux send-keys -t "$suspend_target" Enter
# The window runs the caller's own login shell, whose startup files can take
# many seconds on a loaded machine before the launch line even echoes.
attempt=0
while [ "$attempt" -lt 300 ]; do
  suspend_pane="$(tmux capture-pane -p -t "$suspend_target" 2>/dev/null || true)"
  echo "$suspend_pane" | grep -q "/ commands" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$suspend_pane" | grep -q "/ commands"; then
  echo "$suspend_pane" >&2
  echo "Interactive-shell TUI did not reach its home screen" >&2
  exit 1
fi
suspend_pgid="$(ps -o tpgid= -p "$suspend_shell_pid" 2>/dev/null | tr -d ' ')"
if [ -z "$suspend_pgid" ] || [ "$suspend_pgid" = "$suspend_shell_pid" ]; then
  echo "Could not resolve the foreground Haarness job for Ctrl+Z testing" >&2
  exit 1
fi
tmux send-keys -t "$suspend_target" C-z
attempt=0
while [ "$attempt" -lt 50 ]; do
  suspend_state="$(ps -o state= -p "$suspend_pgid" 2>/dev/null | tr -d ' ' || true)"
  shell_tpgid="$(ps -o tpgid= -p "$suspend_shell_pid" 2>/dev/null | tr -d ' ' || true)"
  case "$suspend_state" in
    T*) [ "$shell_tpgid" = "$suspend_shell_pid" ] && break ;;
  esac
  attempt=$((attempt + 1))
  sleep 0.1
done
case "$suspend_state" in
  T*) ;;
  *)
    echo "Ctrl+Z did not stop the Haarness process group (state=$suspend_state)" >&2
    exit 1
    ;;
esac
if [ "$shell_tpgid" != "$suspend_shell_pid" ]; then
  echo "Ctrl+Z did not return foreground control to the shell" >&2
  exit 1
fi
tmux send-keys -l -t "$suspend_target" 'fg'
tmux send-keys -t "$suspend_target" Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  suspend_pane="$(tmux capture-pane -p -t "$suspend_target" 2>/dev/null || true)"
  echo "$suspend_pane" | grep -q "/ commands" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
tmux send-keys -l -t "$suspend_target" 'suspend resumed'
tmux send-keys -t "$suspend_target" Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  suspend_pane="$(tmux capture-pane -p -t "$suspend_target" 2>/dev/null || true)"
  echo "$suspend_pane" | grep -q "Echo: suspend resumed" && echo "$suspend_pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$suspend_pane" | grep -q "Echo: suspend resumed"; then
  echo "$suspend_pane" >&2
  echo "Haarness did not redraw and accept input after fg" >&2
  exit 1
fi
tmux send-keys -t "$suspend_target" C-c C-c
attempt=0
while [ "$attempt" -lt 50 ]; do
  shell_tpgid="$(ps -o tpgid= -p "$suspend_shell_pid" 2>/dev/null | tr -d ' ' || true)"
  [ "$shell_tpgid" = "$suspend_shell_pid" ] && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ "$shell_tpgid" != "$suspend_shell_pid" ]; then
  echo "Haarness did not return control to the interactive shell on exit" >&2
  exit 1
fi
tmux send-keys -l -t "$suspend_target" "printf 'SUSPEND_SHELL_OK\\n'"
tmux send-keys -t "$suspend_target" Enter
sleep 0.2
suspend_pane="$(tmux capture-pane -p -t "$suspend_target" 2>/dev/null || true)"
if ! echo "$suspend_pane" | grep -q "SUSPEND_SHELL_OK"; then
  echo "$suspend_pane" >&2
  echo "Terminal state was not usable after Haarness exited" >&2
  exit 1
fi
tmux kill-window -t "$suspend_target"

tmux send-keys -t "$session":0.0 "/"
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "/models" && echo "$pane" | grep -q "/providers" && echo "$pane" | grep -q "/new" && echo "$pane" | grep -q "/sessions" && echo "$pane" | grep -q "/resume" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "/models" || ! echo "$pane" | grep -q "/providers" || ! echo "$pane" | grep -q "/new" || ! echo "$pane" | grep -q "/sessions" || ! echo "$pane" | grep -q "/resume"; then
  echo "$pane" >&2
  echo "Slash command autocomplete did not render" >&2
  exit 1
fi

# Tab/window focus changes can make terminal emulators silently drop DEC
# mouse modes. Feed the real focus-out/focus-in events, then sweep several
# inline command rows. Every motion must move the bright selection, not only
# the first event after launch.
focus_cycle="$(printf '\033[O\033[I')"
tmux send-keys -l -t "$session":0.0 "$focus_cycle"
for command in models providers sessions; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  command_row="$(echo "$pane" | awk -v command="/$command" 'index($0, command) { print NR; exit }')"
  if [ -z "$command_row" ]; then
    echo "$pane" >&2
    echo "Could not locate /$command for continuous hover testing" >&2
    exit 1
  fi
  mouse_move="$(printf '\033[<35;16;%sM' "$command_row")"
  tmux send-keys -l -t "$session":0.0 "$mouse_move"
  sleep 0.1
  styled_row="$(tmux capture-pane -p -e -t "$session":0.0 2>/dev/null | sed -n "${command_row}p")"
  if ! echo "$styled_row" | grep -q "48;2;234;229;221m"; then
    echo "$styled_row" >&2
    echo "Mouse motion did not track /$command after terminal focus recovery" >&2
    exit 1
  fi
done

# Enter activates the highlighted completion, even when the user only typed a
# prefix. This is a real terminal regression for the old `/mod` -> literal
# prompt failure rather than an input-handler-only assertion.
tmux send-keys -t "$session":0.0 C-u
tmux send-keys -l -t "$session":0.0 '/mod'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Select model" && echo "$pane" | grep -q "active" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Select model" || ! echo "$pane" | grep -q "active"; then
  echo "$pane" >&2
  echo "Enter on the /mod completion did not open Models" >&2
  exit 1
fi
if echo "$pane" | grep -q "Echo: /mod"; then
  echo "$pane" >&2
  echo "Enter on the /mod completion sent the literal prefix" >&2
  exit 1
fi

# Any-motion SGR tracking moves modal focus before a click. Hover the guaranteed
# test-provider row and verify the real rendered cell background changes to the
# shared selected-row color. Filter first so the check remains viewport-agnostic
# without depending on an optional locally installed provider model.
tmux send-keys -l -t "$session":0.0 'echo-1'
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Fq 'Search  echo-1' \
    && echo "$pane" | grep -Eq 'echo/(echo-1|Echo 1)' \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
model_row="$(echo "$pane" | awk '/echo\/echo-1/ { print NR; exit }')"
if [ -z "$model_row" ]; then
  echo "$pane" >&2
  echo "Could not locate a model row for hover testing" >&2
  exit 1
fi
mouse_move="$(printf '\033[<35;16;%sM' "$model_row")"
tmux send-keys -l -t "$session":0.0 "$mouse_move"
sleep 0.2
styled_row="$(tmux capture-pane -p -e -t "$session":0.0 2>/dev/null | sed -n "${model_row}p")"
if ! echo "$styled_row" | grep -q "48;2;234;229;221m"; then
  echo "$styled_row" >&2
  echo "Mouse motion did not highlight the hovered modal row" >&2
  exit 1
fi

tmux send-keys -t "$session":0.0 Escape

# Dynamic workflows are a first-class command, not only a hidden model tool.
# Prove static discovery, the searchable dashboard, and an actual portable run
# through the same server used by the TUI.
tmux send-keys -t "$session":0.0 C-u
tmux send-keys -l -t "$session":0.0 '/workflow'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Workflows" && echo "$pane" | grep -q "tmux-demo" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Workflows" || ! echo "$pane" | grep -q "tmux-demo"; then
  echo "$pane" >&2
  echo "The /workflow dashboard did not discover the project fixture" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape
tmux send-keys -t "$session":0.0 C-u
tmux send-keys -l -t "$session":0.0 '/tmux-demo {"value":"tmux-e2e"}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Inspect run" && echo "$pane" | grep -q "Open workflow session" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Inspect run" || ! echo "$pane" | grep -q "Open workflow session"; then
  echo "$pane" >&2
  echo "Dynamic /tmux-demo execution did not open durable run actions" >&2
  exit 1
fi
# The inspector is a live report, not a key/value dump: it names the phase,
# the wall time, and the log tail.
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "started " && echo "$pane" | grep -q "LOGS" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "started " || ! echo "$pane" | grep -q "LOGS"; then
  echo "$pane" >&2
  echo "Inspect run did not open the composed workflow report" >&2
  exit 1
fi
if echo "$pane" | grep -q "+00:00"; then
  echo "$pane" >&2
  echo "The workflow report leaked a machine timestamp" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape
tmux send-keys -t "$session":0.0 Escape

# Session vocabulary is first-class and discoverable.
# `/resume` is an alias of the canonical `/sessions` picker; Right opens the
# reversible management actions for the highlighted conversation.
tmux send-keys -t "$session":0.0 C-u
tmux send-keys -l -t "$session":0.0 '/res'
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "/sessions" && echo "$pane" | grep -q "/resume" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "/sessions" || ! echo "$pane" | grep -q "/resume"; then
  echo "$pane" >&2
  echo "The /res completion did not expose canonical /sessions and its /resume alias" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Sessions" && echo "$pane" | grep -q "current" && echo "$pane" | grep -q "enter resume" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Sessions" || ! echo "$pane" | grep -q "esc close" || ! echo "$pane" | grep -q "current" || ! echo "$pane" | grep -q "enter resume" || ! echo "$pane" | grep -q "→ actions"; then
  echo "$pane" >&2
  echo "/resume alias did not open the annotated Sessions modal" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Right
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Fork and resume" && echo "$pane" | grep -q "Copy session ID" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Fork and resume" || ! echo "$pane" | grep -q "Rename" || ! echo "$pane" | grep -q "Archive" || ! echo "$pane" | grep -q "Copy session ID"; then
  echo "$pane" >&2
  echo "Sessions modal did not expose safe management actions" >&2
  exit 1
fi
if echo "$pane" | grep -q "Delete"; then
  echo "$pane" >&2
  echo "Sessions modal exposed an unsupported irreversible delete action" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape

tmux send-keys -t "$session":0.0 "/plan" Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq "plan[[:space:]]+echo/(echo-1|Echo 1)" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Eq "plan[[:space:]]+echo/(echo-1|Echo 1)"; then
  echo "$pane" >&2
  echo "/plan did not switch the active agent" >&2
  exit 1
fi

# `/goal <objective>` is a real persistent runtime mode, not a prompt alias.
# A deliberately slow provider turn gives the management surface a stable
# active state in which to prove pause/resume before the negotiated goal run.
tmux send-keys -l -t "$session":0.0 '/goal ::mdslow'
tmux send-keys -t "$session":0.0 Enter
tmux send-keys -l -t "$session":0.0 '/goal'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Refresh status" && echo "$pane" | grep -q "Pause" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Refresh status" || ! echo "$pane" | grep -q "Pause"; then
  echo "$pane" >&2
  echo "Bare /goal did not open active goal management" >&2
  exit 1
fi
# Pause and resume from the same manager surface. Each successful action
# returns to the manager so the changed state is immediately legible.
tmux send-keys -t "$session":0.0 Down Down Down Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Resume" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Resume"; then
  echo "$pane" >&2
  echo "Goal manager did not expose Resume after pausing" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Down Down Down Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Pause" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Pause"; then
  echo "$pane" >&2
  echo "Goal manager did not return to active state after resuming" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape
# Wait for Escape to close the modal before typing the next slash command.
# Back-to-back bytes can otherwise be interpreted as an Alt key sequence.
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Refresh status" || break
  attempt=$((attempt + 1))
  sleep 0.1
done
tmux send-keys -l -t "$session":0.0 '/goal clear'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Keep goal" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
tmux send-keys -t "$session":0.0 Down Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
tmux send-keys -l -t "$session":0.0 '/goal'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "No active goal" && echo "$pane" | grep -q "Start goal" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "No active goal" || ! echo "$pane" | grep -q "Start goal"; then
  echo "$pane" >&2
  echo "Preflight goal did not clear after pause/resume validation" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape

# Negotiate two criteria through the actual multi-select UI. Echo emits the
# model's criteria proposal and evidence-backed completion in one ordered tool
# batch, so the fixture proves both boundaries without racing its deliberately
# zero-latency autonomous continuations.
goal_command='/goal !multi [{"name":"propose_goal_criteria","arguments":{"criteria":["tmux tests pass","tmux UI approved"],"prompt":"Approve tmux criteria?","label":"Tmux criteria"}},{"name":"update_goal","arguments":{"status":"complete","reason":"tmux proof","criteria_evidence":[{"index":1,"evidence":"tmux test passed"},{"index":2,"evidence":"criteria UI approved"}],"next_step_question":{"title":"Tmux goal achieved","edit":{"label":"Edit","description":"Refine this goal."},"new":{"label":"New","description":"Start another goal."},"clear":{"label":"Clear","description":"Remove the goal."},"dismiss":{"label":"Dismiss","description":"Close this dialog."}}}}]'
tmux send-keys -l -t "$session":0.0 "$goal_command"
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 120 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Approve tmux criteria?" && echo "$pane" | grep -q "tmux tests pass" && echo "$pane" | grep -q "space toggle" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Approve tmux criteria?" || ! echo "$pane" | grep -q "space toggle"; then
  echo "$pane" >&2
  echo "Goal criteria negotiation did not open the multi-select question" >&2
  exit 1
fi
# Select both proposed criteria. Clicks and Space only toggle; Enter is the
# explicit confirmation boundary.
tmux send-keys -t "$session":0.0 Space Down Space Enter
# Wait for the asynchronous question reply to retire the inline selector
# before injecting the next prompt. This proves the real transition instead
# of relying on tmux typeahead timing.
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  ! echo "$pane" | grep -Fq "? Tmux criteria" \
    && ! echo "$pane" | grep -Fq "[ ] 1. tmux tests pass" \
    && ! echo "$pane" | grep -q "space toggle" \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if echo "$pane" | grep -Fq "? Tmux criteria" \
  || echo "$pane" | grep -Fq "[ ] 1. tmux tests pass" \
  || echo "$pane" | grep -q "space toggle"; then
  echo "$pane" >&2
  echo "Goal criteria answer left a stale multi-select question in the transcript" >&2
  exit 1
fi
attempt=0
while [ "$attempt" -lt 160 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Tmux goal achieved" && echo "$pane" | grep -q "Dismiss" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Tmux goal achieved" || ! echo "$pane" | grep -q "Dismiss"; then
  echo "$pane" >&2
  echo "Goal completion did not open the model-authored next-step picker" >&2
  exit 1
fi
# Goal action dialogs do not filter. Typing cannot displace Dismiss, the safe
# completion default, or leak into the composer.
tmux send-keys -l -t "$session":0.0 'zz_goal_modal_probe'
sleep 0.3
pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
if echo "$pane" | grep -q "zz_goal_modal_probe"; then
  echo "$pane" >&2
  echo "Typing leaked into the non-filterable completion picker" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Enter
tmux send-keys -l -t "$session":0.0 '/goal clear'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Clear goal" && echo "$pane" | grep -q "Keep goal" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Clear goal" || ! echo "$pane" | grep -q "Keep goal"; then
  echo "$pane" >&2
  echo "/goal clear did not open its cancel-first confirmation" >&2
  exit 1
fi
# Typing cannot filter away Keep goal. Enter cancels and returns to the goal
# manager, preserving the completed goal.
tmux send-keys -l -t "$session":0.0 'clear'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Refresh status" && echo "$pane" | grep -q "Clear" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Refresh status"; then
  echo "$pane" >&2
  echo "Safe-default clear cancellation did not return to goal management" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape
# Wait for Escape to close the modal before typing the next slash command.
# Back-to-back bytes can otherwise be interpreted as an Alt key sequence.
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Refresh status" || break
  attempt=$((attempt + 1))
  sleep 0.1
done
tmux send-keys -l -t "$session":0.0 '/goal clear'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Keep goal" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
# Escape is Back for the nested confirmation, not Close for the whole stack.
tmux send-keys -t "$session":0.0 Escape
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Refresh status" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Refresh status"; then
  echo "$pane" >&2
  echo "Escape from clear confirmation did not return to goal management" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape
# Wait for Escape to close the modal before typing the next slash command.
# Back-to-back bytes can otherwise be interpreted as an Alt key sequence.
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Refresh status" || break
  attempt=$((attempt + 1))
  sleep 0.1
done
tmux send-keys -l -t "$session":0.0 '/goal clear'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Keep goal" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
tmux send-keys -t "$session":0.0 Down Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Goal cleared" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Goal cleared"; then
  echo "$pane" >&2
  echo "Confirmed goal clear did not remove the standing goal" >&2
  exit 1
fi

tmux send-keys -t "$session":0.0 "hello from tmux" Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Echo: hello from tmux" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Echo: hello from tmux"; then
  echo "$pane" >&2
  echo "Prompt did not stream an Echo response" >&2
  exit 1
fi
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "▸ Echo: hello from tmux" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "▸ Echo: hello from tmux"; then
  echo "$pane" >&2
  echo "Assistant reply marker did not render" >&2
  exit 1
fi
if ! echo "$pane" | grep -q "▎ hello from tmux"; then
  echo "$pane" >&2
  echo "User prompt block did not render its bar" >&2
  exit 1
fi

# Drag-select real transcript cells. Mouse release must copy immediately;
# preserve and restore the user's clipboard around the test.
if [ "$clipboard_saved" -eq 1 ]; then
  selection_row="$(echo "$pane" | awk '/▸ Echo: hello from tmux/ { print NR; exit }')"
  if [ -z "$selection_row" ]; then
    echo "$pane" >&2
    echo "Could not locate assistant text for selection testing" >&2
    exit 1
  fi
  printf 'haarness-selection-sentinel' | pbcopy
  mouse_down="$(printf '\033[<0;3;%sM' "$selection_row")"
  mouse_drag="$(printf '\033[<32;18;%sM' "$selection_row")"
  mouse_up="$(printf '\033[<0;18;%sm' "$selection_row")"
  tmux send-keys -l -t "$session":0.0 "$mouse_down"
  tmux send-keys -l -t "$session":0.0 "$mouse_drag"
  tmux send-keys -l -t "$session":0.0 "$mouse_up"
  attempt=0
  while [ "$attempt" -lt 30 ]; do
    clipboard="$(pbpaste 2>/dev/null || true)"
    echo "$clipboard" | grep -q "Echo: hello" && break
    attempt=$((attempt + 1))
    sleep 0.1
  done
  if ! echo "$clipboard" | grep -q "Echo: hello"; then
    echo "Mouse selection was not copied on release" >&2
    exit 1
  fi
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  if ! echo "$pane" | grep -q "Copied selection"; then
    echo "$pane" >&2
    echo "Selection copy toast did not render" >&2
    exit 1
  fi
fi

# Preserve the provider's semantic stream order in the actual terminal: prose
# introducing a tool must render above that tool, matching Haarness's
# text-start/tool-start part ordering. This also protects reopened transcripts
# because the runtime now persists the same boundary order.
tmux send-keys -l -t "$session":0.0 ' !preface grep {"pattern":"haarness","path":"README.md"}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "inspect the repository first" \
    && echo "$pane" | grep -q "Tool grep =>" \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
preface_row="$(echo "$pane" | awk '/inspect the repository first/ { print NR; exit }')"
grep_row="$(echo "$pane" | awk '/grep haarness in README.md/ { print NR; exit }')"
if [ -z "$preface_row" ] || [ -z "$grep_row" ] || [ "$preface_row" -ge "$grep_row" ]; then
  echo "$pane" >&2
  echo "Assistant preface did not render before its grep activity" >&2
  exit 1
fi

tmux send-keys -l -t "$session":0.0 ' !tool skill {"name":"demo"}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Tool skill =>" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Tool skill =>"; then
  echo "$pane" >&2
  echo "The skill tool did not load the demo skill through the agent loop" >&2
  exit 1
fi
if ! echo "$pane" | grep -q "● skill demo"; then
  echo "$pane" >&2
  echo "Successful exploratory output did not collapse into a clean call row" >&2
  exit 1
fi

# Exercise crossterm's real SGR mouse path through tmux. Coordinates in SGR
# reports are one-based; column 4 is inside the transcript hit region.
tool_row="$(echo "$pane" | awk '/● skill demo/ { print NR; exit }')"
if [ -z "$tool_row" ]; then
  echo "$pane" >&2
  echo "Could not locate the compact tool row for mouse testing" >&2
  exit 1
fi
mouse_down="$(printf '\033[<0;4;%sM' "$tool_row")"
mouse_up="$(printf '\033[<0;4;%sm' "$tool_row")"
tmux send-keys -l -t "$session":0.0 "$mouse_down"
tmux send-keys -l -t "$session":0.0 "$mouse_up"
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "▏ Skill directory:" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "▏ Skill directory:"; then
  echo "$pane" >&2
  echo "Clicking the compact tool row did not reveal its paired output" >&2
  exit 1
fi

# Expansion changes the frame height, so resolve the call row again before
# clicking it a second time.
tool_row="$(echo "$pane" | awk '/● skill demo/ { print NR; exit }')"
if [ -z "$tool_row" ]; then
  echo "$pane" >&2
  echo "Could not locate the expanded tool row for mouse testing" >&2
  exit 1
fi
mouse_down="$(printf '\033[<0;4;%sM' "$tool_row")"
mouse_up="$(printf '\033[<0;4;%sm' "$tool_row")"
tmux send-keys -l -t "$session":0.0 "$mouse_down"
tmux send-keys -l -t "$session":0.0 "$mouse_up"
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
! echo "$pane" | grep -q "▏ Skill directory:" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if echo "$pane" | grep -q "▏ Skill directory:"; then
  echo "$pane" >&2
  echo "Clicking the expanded tool row did not collapse its output" >&2
  exit 1
fi

# Composer editing: cursor movement + mid-text insertion.
tmux send-keys -t "$session":0.0 "hello"
tmux send-keys -t "$session":0.0 Left Left
tmux send-keys -t "$session":0.0 "X"
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "helXlo" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "helXlo"; then
  echo "$pane" >&2
  echo "Composer cursor editing did not work" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape
tmux send-keys -t "$session":0.0 C-u

# Haarness parity: one Ctrl+C clears a non-empty draft, does not interrupt or
# quit, and says so in the status line. Ambient feedback never pops a card.
tmux send-keys -l -t "$session":0.0 'clear this draft once'
tmux send-keys -t "$session":0.0 C-c
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Input cleared" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if echo "$pane" | grep -q "clear this draft once" || ! echo "$pane" | grep -q "Input cleared"; then
  echo "$pane" >&2
  echo "Single Ctrl+C did not clear the draft and show feedback" >&2
  exit 1
fi
notice_row="$(echo "$pane" | awk '/Input cleared/ { print NR; exit }')"
if [ -z "$notice_row" ] || [ "$notice_row" -lt 20 ]; then
  echo "$pane" >&2
  echo "Input-cleared feedback was not in the status line" >&2
  exit 1
fi

# History recall: the earlier submitted prompt returns on Up.
tmux send-keys -t "$session":0.0 Up
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "▏ .*hello from tmux" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "hello from tmux"; then
  echo "$pane" >&2
  echo "Prompt history recall did not surface the previous prompt" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 C-u

# Dot-relative browsing uses the workspace as its base, keeps `../` visible,
# and selecting a directory immediately drills into its next level.
tmux send-keys -l -t "$session":0.0 '@../'
attempt=0
while [ "$attempt" -lt 60 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "files · local paths" && echo "$pane" | grep -q "../00-dot-nav/" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "../00-dot-nav/"; then
  echo "$pane" >&2
  echo "@../ did not browse the workspace parent" >&2
  exit 1
fi
tmux send-keys -l -t "$session":0.0 '00'
sleep 0.2
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 60 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "@../00-dot-nav/" && echo "$pane" | grep -q "../00-dot-nav/parent image.png" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "../00-dot-nav/parent image.png"; then
  echo "$pane" >&2
  echo "Selecting a relative directory did not open its children" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq "img[[:space:]]+parent image.png" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Eq "img[[:space:]]+parent image.png"; then
  echo "$pane" >&2
  echo "Relative parent-path media selection did not attach" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 C-c

# Absolute @-path completion: this is the exact `/Users/...`-style flow that
# previously showed a fake instruction row and did nothing on Enter.
tmux send-keys -l -t "$session":0.0 "@$test_root/ws/drop"
attempt=0
while [ "$attempt" -lt 60 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "files · local paths" && echo "$pane" | grep -q "dropped image.png" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "dropped image.png"; then
  echo "$pane" >&2
  echo "Absolute @-path completion did not find the real media file" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq "img[[:space:]]+dropped image.png" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Eq "img[[:space:]]+dropped image.png"; then
  echo "$pane" >&2
  echo "Selecting the absolute media path did not attach it" >&2
  exit 1
fi
# Ctrl+C clears attachments as well as text so later media tests start clean.
tmux send-keys -t "$session":0.0 C-c
sleep 0.2
pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
if echo "$pane" | grep -Eq "img[[:space:]]+dropped image.png"; then
  echo "$pane" >&2
  echo "Ctrl+C did not clear the completed media attachment" >&2
  exit 1
fi

# Large paste collapses to a placeholder.
paste_payload="$(seq 1 200 | sed 's/^/pasted-line-/')"
tmux set-buffer -b haarness-e2e "$paste_payload"
tmux paste-buffer -p -b haarness-e2e -t "$session":0.0
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "pasted 200 lines" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "pasted 200 lines"; then
  echo "$pane" >&2
  echo "Large paste did not collapse to a placeholder" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 C-u

# Scrollback: a long response scrolls; PageUp reveals early markers.
tmux send-keys -l -t "$session":0.0 '::long 120'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "marker-120" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
# Advance a page at a time and stop on the target. A fixed burst can legally
# jump past marker 1 when earlier E2E sections add transcript height.
attempt=0
while [ "$attempt" -lt 30 ]; do
  tmux send-keys -t "$session":0.0 PageUp
  sleep 0.1
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "line 1 marker-1" && break
  attempt=$((attempt + 1))
done
if ! echo "$pane" | grep -q "line 1 marker-1"; then
  echo "$pane" >&2
  echo "PageUp did not reveal early scrollback content" >&2
  exit 1
fi
if echo "$pane" | grep -q "line 120 marker-120"; then
  echo "$pane" >&2
  echo "Scrolled view should not still show the transcript bottom" >&2
  exit 1
fi
if ! echo "$pane" | grep -q "scrolled · end to resume"; then
  echo "$pane" >&2
  echo "Scrolled status did not render the bounded navigation hint" >&2
  exit 1
fi
# Overscroll must clamp at the actual first row. Once clamped, additional
# PageUp input cannot keep accumulating a hidden offset or move the frame.
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  tmux send-keys -t "$session":0.0 PageUp
done
# Let the earlier input-cleared toast expire so this exact-frame comparison is
# about scroll state only.
sleep 2.5
at_scroll_limit="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
for _ in 1 2 3 4 5 6 7 8 9 10; do
  tmux send-keys -t "$session":0.0 PageUp
done
sleep 1
past_scroll_limit="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
if [ "$at_scroll_limit" != "$past_scroll_limit" ]; then
  echo "$past_scroll_limit" >&2
  echo "Repeated PageUp changed the frame after reaching the scroll limit" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 End
sleep 1
pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
if ! echo "$pane" | grep -q "marker-120"; then
  echo "$pane" >&2
  echo "End did not return to the transcript bottom" >&2
  exit 1
fi

# Manual follow suspension is a full streaming lifecycle, not merely a static
# PageUp check. The provider first emits an unclosed Markdown link, closes it
# (which contracts the rendered block), then grows the response again. The
# exact durable marker window must remain fixed until End explicitly resumes.
tmux send-keys -l -t "$session":0.0 '::scrollstream'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "STREAM_REFLOW_STAGE_ONE" && ! echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
if ! echo "$pane" | grep -q "STREAM_REFLOW_STAGE_ONE" || echo "$pane" | grep -q "Ready"; then
  echo "$pane" >&2
  echo "Could not establish the live Markdown reflow fixture" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 PageUp
sleep 0.2
pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
stream_anchor_before="$(echo "$pane" | sed -nE 's/.*(line [0-9]+ marker-[0-9]+).*/\1/p')"
if [ -z "$stream_anchor_before" ] || ! echo "$pane" | grep -q "scrolled · end to resume"; then
  echo "$pane" >&2
  echo "PageUp did not pause the live stream on a durable transcript window" >&2
  exit 1
fi
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
stream_anchor_after="$(echo "$pane" | sed -nE 's/.*(line [0-9]+ marker-[0-9]+).*/\1/p')"
if [ "$stream_anchor_before" != "$stream_anchor_after" ]; then
  echo "$pane" >&2
  echo "Streaming Markdown reflow moved the manually paused transcript window" >&2
  exit 1
fi
if ! echo "$pane" | grep -q "scrolled · end to resume" || echo "$pane" | grep -q "STREAM_REFLOW_DONE"; then
  echo "$pane" >&2
  echo "Completed streaming resumed follow mode without an explicit return to bottom" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 End
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "STREAM_REFLOW_DONE" && ! echo "$pane" | grep -q "scrolled · end to resume" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
if ! echo "$pane" | grep -q "STREAM_REFLOW_DONE" || echo "$pane" | grep -q "scrolled · end to resume"; then
  echo "$pane" >&2
  echo "End did not resume live transcript following at the latest output" >&2
  exit 1
fi

# Markdown rendering: bold text without literal markers.
tmux send-keys -l -t "$session":0.0 '::md'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "fn main" && echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if echo "$pane" | grep -q '\*\*bold\*\*'; then
  echo "$pane" >&2
  echo "Markdown bold markers leaked into the transcript" >&2
  exit 1
fi
if ! echo "$pane" | grep -q "bold"; then
  echo "$pane" >&2
  echo "Markdown content did not render" >&2
  exit 1
fi

# Delayed, multi-chunk Markdown proves formatting happens during streaming,
# not only when the final durable assistant item arrives.
tmux send-keys -l -t "$session":0.0 '::mdslow'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 30 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Live markdown" && ! echo "$pane" | grep -q "streams while working" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
if ! echo "$pane" | grep -q "Live markdown" || echo "$pane" | grep -q '\*\*Live markdown\*\*'; then
  echo "$pane" >&2
  echo "Live Markdown was not rendered before the stream completed" >&2
  exit 1
fi
if echo "$pane" | grep -q "Ready"; then
  echo "$pane" >&2
  echo "Slow Markdown fixture completed before the live-render assertion" >&2
  exit 1
fi
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "streams while working" && echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "streams while working" || ! echo "$pane" | grep -q "Ready"; then
  echo "$pane" >&2
  echo "Slow Markdown stream did not finish cleanly" >&2
  exit 1
fi

# Running-task Escape parity: the first press only arms cancellation; the
# second press for the same turn performs it.
tmux send-keys -l -t "$session":0.0 '::mdslow'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 20 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Live markdown" && ! echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
tmux send-keys -t "$session":0.0 Escape
attempt=0
while [ "$attempt" -lt 10 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "esc again to interrupt" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
if ! echo "$pane" | grep -q "esc again to interrupt"; then
  echo "$pane" >&2
  echo "First Escape interrupted instead of arming confirmation" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape
attempt=0
while [ "$attempt" -lt 10 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq "stopping…|interrupt requested" && break
  attempt=$((attempt + 1))
  sleep 0.02
done
if ! echo "$pane" | grep -Eq "stopping…|interrupt requested|Ready"; then
  echo "$pane" >&2
  echo "Confirmed Escape interrupt gave no immediate feedback" >&2
  exit 1
fi
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Ready"; then
  echo "$pane" >&2
  echo "Second Escape did not interrupt the active turn" >&2
  exit 1
fi

# Ctrl+C uses the same two-press active-turn confirmation, but the confirmed
# request must return to rendering immediately instead of waiting on HTTP or a
# provider's next stream event.
tmux send-keys -l -t "$session":0.0 '::mdslow'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 20 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Live markdown" && ! echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
tmux send-keys -t "$session":0.0 C-c
attempt=0
while [ "$attempt" -lt 10 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "again to interrupt" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
if ! echo "$pane" | grep -q "again to interrupt"; then
  echo "$pane" >&2
  echo "First active Ctrl+C did not arm interruption" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 C-c
attempt=0
while [ "$attempt" -lt 10 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq "stopping…|interrupt requested|Ready" && break
  attempt=$((attempt + 1))
  sleep 0.02
done
if ! echo "$pane" | grep -Eq "stopping…|interrupt requested|Ready"; then
  echo "$pane" >&2
  echo "Confirmed Ctrl+C interrupt blocked without feedback" >&2
  exit 1
fi
attempt=0
while [ "$attempt" -lt 20 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
if ! echo "$pane" | grep -q "Ready"; then
  echo "$pane" >&2
  echo "Confirmed Ctrl+C did not stop the silent stream promptly" >&2
  exit 1
fi
# Question tool: arrows move visible focus and Enter submits that choice.
tmux send-keys -l -t "$session":0.0 ' !tool question {"prompt":"Pick one","choices":["red","blue"]}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "? Pick one" && echo "$pane" | grep -q "1. red" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "1. red"; then
  echo "$pane" >&2
  echo "Question choices did not render" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Down Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Tool question => blue" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Tool question => blue"; then
  echo "$pane" >&2
  echo "Arrow selection plus Enter did not resolve the highlighted question choice" >&2
  exit 1
fi

# TODO updates live only in the wide sidebar; terminal-only lists disappear.
tmux send-keys -l -t "$session":0.0 ' !tool todo {"items":[{"content":"tmux todo active","status":"in_progress","priority":"high"}]}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Tool todo => Todo list updated (1 items)" && echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if echo "$pane" | grep -q "# Todos" || ! echo "$pane" | grep -q "Tool todo => Todo list updated (1 items)"; then
  echo "$pane" >&2
  echo "Todo update did not finish or leaked a transcript block" >&2
  exit 1
fi
tmux resize-window -t "$session":0 -x 140 -y 30
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "▾ todo" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "▾ todo"; then
  echo "$pane" >&2
  echo "Current actionable todo list did not appear in the wide sidebar" >&2
  exit 1
fi
tmux send-keys -l -t "$session":0.0 ' !tool todo {"items":[{"content":"tmux todo active","status":"completed","priority":"high"},{"content":"tmux todo done","status":"completed","priority":"low"}]}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Tool todo => Todo list updated (2 items)" && ! echo "$pane" | grep -q "▾ todo" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if echo "$pane" | grep -q "▾ todo"; then
  echo "$pane" >&2
  echo "Completed todo list remained pinned in the sidebar" >&2
  exit 1
fi
tmux resize-window -t "$session":0 -x 100 -y 30

# Reasoning: Compact (the default) keeps a thought to one "Thought for" row;
# Ctrl+T shows every provider-visible thought in full, without disclosure
# chrome.
tmux send-keys -l -t "$session":0.0 '::reason deeply'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "▸ Thought for" && echo "$pane" | grep -q "Reasoned reply" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "▸ Thought for" || echo "$pane" | grep -q "Considering deeply"; then
  echo "$pane" >&2
  echo "Compact reasoning did not settle as one Thought row" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 C-t
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Considering deeply" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Considering deeply"; then
  echo "$pane" >&2
  echo "Durable reasoning did not render" >&2
  exit 1
fi
if echo "$pane" | grep -q "thinking · click"; then
  echo "$pane" >&2
  echo "Reasoning rendered unnecessary disclosure chrome" >&2
  exit 1
fi

# /reasoning opens a selector modal instead of demanding typed arguments.
tmux send-keys -t "$session":0.0 "/reasoning" Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "provider default" && echo "$pane" | grep -q "Thinking display: Show" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "provider default" || echo "$pane" | grep -q "Deep thinking"; then
  echo "$pane" >&2
  echo "/reasoning did not respect the Echo model capability domain" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape
sleep 0.2

# Live theme switching: the file theme repaints the background, and returning
# to editorial restores the identity RGB.
tmux send-keys -t "$session":0.0 "/themes e2e dark" Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  styled_pane="$(tmux capture-pane -p -e -t "$session":0.0 2>/dev/null || true)"
  echo "$styled_pane" | grep -q "48;2;16;32;48m" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$styled_pane" | grep -q "48;2;16;32;48m"; then
  echo "$styled_pane" >&2
  echo "Theme switch did not repaint the background" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 "/themes editorial dark" Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  styled_pane="$(tmux capture-pane -p -e -t "$session":0.0 2>/dev/null || true)"
  echo "$styled_pane" | grep -q "48;2;15;14;13m" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$styled_pane" | grep -q "48;2;15;14;13m"; then
  echo "$styled_pane" >&2
  echo "Switching back did not restore the editorial background" >&2
  exit 1
fi

tmux send-keys -t "$session":0.0 "/build" Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq "build[[:space:]]+echo/(echo-1|Echo 1)" && break
  attempt=$((attempt + 1))
  sleep 0.1
done

# Shell mode must not await a permission-gated HTTP request inside the sole UI
# event loop. The approval appears, remains answerable, and the durable result
# arrives without freezing input/rendering.
tmux send-keys -l -t "$session":0.0 '!'
tmux send-keys -l -t "$session":0.0 'touch shell-e2e.txt'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Run a shell command: touch shell-e2e.txt" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Run a shell command: touch shell-e2e.txt"; then
  echo "$pane" >&2
  echo "Permission-gated shell command blocked the TUI event loop" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 y
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  [ -f "$test_root/ws/shell-e2e.txt" ] && echo "$pane" | grep -q "Shell finished" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ ! -f "$test_root/ws/shell-e2e.txt" ] || ! echo "$pane" | grep -q "Shell finished"; then
  echo "$pane" >&2
  echo "Approved shell command did not finish and notify" >&2
  exit 1
fi

tmux send-keys -l -t "$session":0.0 ' !tool write {"path":"e2e.txt","content":"approved by tmux"}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq "Edit a file: .*e2e.txt" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Eq "Edit a file: .*e2e.txt"; then
  echo "$pane" >&2
  echo "Mutating tool did not prompt for permission" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 "y"
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Tool write =>" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Tool write =>"; then
  echo "$pane" >&2
  echo "Approved write tool did not run" >&2
  exit 1
fi
if [ "$(cat "$test_root/ws/e2e.txt" 2>/dev/null)" != "approved by tmux" ]; then
  echo "Approved write did not produce the expected file" >&2
  exit 1
fi

# The review viewer is a full-screen surface, not a card over the transcript:
# a file tree, a numbered patch, live key hints, and every source reachable
# from inside it. Each source is checked independently so a missing selector
# cannot silently degrade to the working-tree result.
expect_review() {
  review_label=$1
  review_file=$2
  review_what=$3
  attempt=0
  while [ "$attempt" -lt 80 ]; do
    pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
    echo "$pane" | grep -q "Diff  $review_label" \
      && echo "$pane" | grep -q "$review_file" \
      && break
    attempt=$((attempt + 1))
    sleep 0.1
  done
  if ! echo "$pane" | grep -q "Diff  $review_label" || ! echo "$pane" | grep -q "$review_file"; then
    echo "$pane" >&2
    echo "$review_what" >&2
    exit 1
  fi
}

tmux send-keys -t "$session":0.0 "/diff last" Enter
expect_review "last turn" "e2e.txt" "/diff last did not render the completed write"
# The surface owns the terminal: hints on the last row, the conversation gone.
pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
if ! echo "$pane" | grep -q "n/p change"; then
  echo "$pane" >&2
  echo "the review viewer did not render its key hints" >&2
  exit 1
fi
if ! echo "$pane" | grep -q "approved by tmux"; then
  echo "$pane" >&2
  echo "the review viewer did not render the changed line" >&2
  exit 1
fi

# ? opens the shortcut list and any key returns to the review.
tmux send-keys -t "$session":0.0 "?"
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Review shortcuts" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Review shortcuts"; then
  echo "$pane" >&2
  echo "? did not open the review shortcut list" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape
expect_review "last turn" "e2e.txt" "escape did not return from the shortcut list"

# s cycles the source in place, without retyping the command.
tmux send-keys -t "$session":0.0 "s"
expect_review "working tree" "e2e.txt" "s did not switch the review source"

# A queued line comment travels to the composer when the viewer closes.
tmux send-keys -t "$session":0.0 "c"
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "comment e2e.txt" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "comment e2e.txt"; then
  echo "$pane" >&2
  echo "c did not open a comment on the cursor line" >&2
  exit 1
fi
tmux send-keys -l -t "$session":0.0 "tighten this"
tmux send-keys -t "$session":0.0 Enter
tmux send-keys -t "$session":0.0 "q"
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "tighten this" \
    && echo "$pane" | grep -q "Ask anything\|comment regarding" \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "tighten this"; then
  echo "$pane" >&2
  echo "the queued review comment did not reach the composer" >&2
  exit 1
fi
# Clear the draft so later steps start from an empty composer.
tmux send-keys -t "$session":0.0 C-u

tmux send-keys -t "$session":0.0 "/diff git" Enter
expect_review "working tree" "e2e.txt" "/diff git did not render working-tree changes"
# t hides the file tree; the patch stays.
tmux send-keys -t "$session":0.0 "t"
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "└─" || break
  attempt=$((attempt + 1))
  sleep 0.1
done
if echo "$pane" | grep -q "└─"; then
  echo "$pane" >&2
  echo "t did not hide the review file tree" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 "t"
tmux send-keys -t "$session":0.0 Escape

tmux send-keys -t "$session":0.0 "/diff branch" Enter
expect_review "main branch" "branch-review.txt" "/diff branch did not render the feature-branch change"
tmux send-keys -t "$session":0.0 Escape

# Undo restores the pre-turn workspace; redo brings the change back.
tmux send-keys -t "$session":0.0 "/undo" Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  [ ! -f "$test_root/ws/e2e.txt" ] && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ -f "$test_root/ws/e2e.txt" ]; then
  tmux capture-pane -p -t "$session":0.0 >&2 || true
  echo "/undo did not revert the approved write" >&2
  exit 1
fi
# Undo restores the original user prompt into the composer for editing. A
# slash typed over that untouched draft starts a command instead of appending
# to it, so /redo runs as the undo notice suggests.
tmux send-keys -t "$session":0.0 "/redo" Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  [ -f "$test_root/ws/e2e.txt" ] && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ "$(cat "$test_root/ws/e2e.txt" 2>/dev/null)" != "approved by tmux" ]; then
  tmux capture-pane -p -t "$session":0.0 >&2 || true
  echo "/redo did not restore the reverted file" >&2
  exit 1
fi
# Undo history is legible: the notice says how far back the workspace can go.
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -qi "redone" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -qi "redone"; then
  echo "$pane" >&2
  echo "/redo did not report what it restored" >&2
  exit 1
fi

# Filesystem tools must ask for an external-directory capability before their
# ordinary tool permission, then execute the exact approved path. This is the
# interactive regression for `list /tmp` silently becoming a hard denial.
tmux send-keys -l -t "$session":0.0 " !tool list {\"path\":\"$test_root/external-list\"}"
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Access outside the project" \
    && echo "$pane" | grep -q "/external-list" \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Access outside the project" || ! echo "$pane" | grep -q "/external-list"; then
  echo "$pane" >&2
  echo "External list did not surface an approval prompt" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 y
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Tool list =>" && echo "$pane" | grep -q "visible.txt" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Tool list =>" || ! echo "$pane" | grep -q "visible.txt"; then
  echo "$pane" >&2
  echo "Approved external list did not execute" >&2
  exit 1
fi


# ---------------------------------------------------------------------------
# P0: parallel child agents with live progress, cross-thread approvals, and
# mid-turn steering input.
# ---------------------------------------------------------------------------
# Before any child exists, the wide sidebar gives stable usage information the
# prime slot and suppresses the empty agents section entirely.
tmux resize-window -t "$session":0 -x 140 -y 30
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "▾ usage" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "▾ usage" || echo "$pane" | grep -q "▾ agents" || echo "$pane" | grep -q "none this turn"; then
  echo "$pane" >&2
  echo "Wide sidebar did not prioritize usage or suppress an empty agent roster" >&2
  exit 1
fi

# Keep one actionable todo live while the child agents run. That lets the
# terminal gate prove the intended sidebar priority order with real state.
tmux send-keys -l -t "$session":0.0 ' !tool todo {"items":[{"content":"sidebar priority sentinel","status":"in_progress","priority":"high"}]}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "sidebar priority sentinel" && echo "$pane" | grep -q "▾ todo" && echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "▾ todo" || ! echo "$pane" | grep -q "sidebar priority sentinel"; then
  echo "$pane" >&2
  echo "Actionable todo did not populate the wide sidebar fixture" >&2
  exit 1
fi

tmux send-keys -t "$session":0.0 C-u
tmux send-keys -l -t "$session":0.0 ' !multi [{"name":"task","arguments":{"prompt":"!tool bash {\"command\":\"sleep 8\"}","agent":"general"}},{"name":"task","arguments":{"prompt":"!tool bash {\"command\":\"sleep 8\"}","agent":"general","background":true}}]'
tmux send-keys -t "$session":0.0 Enter
# A child's bash call needs approval; the prompt must surface in the parent
# view even though it lives on the child thread.
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Run a shell command: sleep 8" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Run a shell command: sleep 8"; then
  echo "$pane" >&2
  echo "Child-thread permission prompt never surfaced" >&2
  exit 1
fi
# The approval card is the attention surface while it is on screen: no toast
# repeats it over the conversation, and the prompt below it says how to type.
sleep 0.5
pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
if echo "$pane" | grep -q "Haarness needs approval" \
  || ! echo "$pane" | grep -q "Answer the approval above"; then
  echo "$pane" >&2
  echo "An approval toast covered the conversation while the card was visible" >&2
  exit 1
fi
# One Always answer must cover both identical sibling prompts. The first
# press shows the generalized rules; the second confirms.
tmux send-keys -t "$session":0.0 "a"
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "until the Haarness server stops" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Eq "(bash:)?sleep \*"; then
  echo "$pane" >&2
  echo "Always confirmation did not show the generalized rule" >&2
  exit 1
fi
# The same letter confirms only once the card could be read; pressed at
# typing speed it would be the start of a word.
sleep 0.5
tmux send-keys -t "$session":0.0 "a"
attempt=0
while [ "$attempt" -lt 60 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "▾ agents" \
    && echo "$pane" | grep -Eq "!tool bash" \
    && echo "$pane" | grep -Eq "[Gg]eneral.*bash.*tools" \
    && echo "$pane" | grep -q "running in background" \
    && ! echo "$pane" | grep -Eq "needs approval|waiting for approval|Run a shell command: sleep" \
    && ! echo "$pane" | grep -q "until the Haarness server stops" \
    && ! echo "$pane" | grep -q "Haarness needs approval" \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "▾ agents" \
  || ! echo "$pane" | grep -Eq "!tool bash" \
  || ! echo "$pane" | grep -Eq "[Gg]eneral.*bash.*tools" \
  || ! echo "$pane" | grep -q "running in background" \
  || echo "$pane" | grep -Eq "needs approval|waiting for approval|Run a shell command: sleep" \
  || echo "$pane" | grep -q "until the Haarness server stops" \
  || echo "$pane" | grep -q "Haarness needs approval"; then
  echo "$pane" >&2
  echo "Live child rows did not settle after the permission resolution" >&2
  exit 1
fi
# An executing child is not queued. Its transcript marker must be one of the
# animated working frames, never the hollow queued circle, and the adjacent
# copy must remain explicit for screenshots and terminals where animation is
# subtle. Observe more than one render tick to prove this is live animation
# rather than a different static glyph.
task_row="$(echo "$pane" | awk '/task !tool bash/ { print; exit }')"
if [ -z "$task_row" ] \
  || ! echo "$task_row" | grep -Eq '^[[:space:]]*[⠋⠙⠸⠴⠦⠇][[:space:]]+task[[:space:]]' \
  || echo "$task_row" | grep -q '○'; then
  echo "$pane" >&2
  echo "Active child task did not use the working marker" >&2
  exit 1
fi
if ! echo "$pane" | grep -Eq '[Rr]unning'; then
  echo "$pane" >&2
  echo "Active child task had no textual running label" >&2
  exit 1
fi
if ! echo "$pane" | grep -q 'running in background'; then
  echo "$pane" >&2
  echo "Background child lost its explicit execution mode after live progress" >&2
  exit 1
fi
task_marker="$(echo "$task_row" | sed 's/^[[:space:]]*//' | cut -d ' ' -f 1)"
next_task_marker="$task_marker"
attempt=0
while [ "$attempt" -lt 10 ]; do
  sleep 0.2
  next_pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  next_task_row="$(echo "$next_pane" | awk '/task !tool bash/ { print; exit }')"
  next_task_marker="$(echo "$next_task_row" | sed 's/^[[:space:]]*//' | cut -d ' ' -f 1)"
  [ -n "$next_task_marker" ] && [ "$next_task_marker" != "$task_marker" ] && break
  attempt=$((attempt + 1))
done
if [ -z "$next_task_marker" ] || [ "$next_task_marker" = "$task_marker" ]; then
  echo "$pane" >&2
  echo "Active child task marker did not animate across render ticks" >&2
  exit 1
fi
usage_row="$(echo "$pane" | awk '/▾ usage/ { print NR; exit }')"
todo_row="$(echo "$pane" | awk '/▾ todo/ { print NR; exit }')"
agents_row="$(echo "$pane" | awk '/▾ agents/ { print NR; exit }')"
if [ -z "$usage_row" ] || [ -z "$todo_row" ] || [ -z "$agents_row" ] || [ "$usage_row" -ge "$todo_row" ] || [ "$todo_row" -ge "$agents_row" ]; then
  echo "$pane" >&2
  echo "Sidebar order was not usage, todo, then active agents" >&2
  exit 1
fi

# The child footer gutter assertion below examines the conversation pane as a
# whole row; return to a no-sidebar width so sidebar content cannot masquerade
# as transcript content in that deliberate blank row.
tmux resize-window -t "$session":0 -x 100 -y 30

# Open a genuinely running child, not a synthetic fixture: the footer must
# expose its lifecycle while the inspection pane remains strictly read-only.
tmux send-keys -t "$session":0.0 C-o
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "child agents" && echo "$pane" | grep -q "running" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "child agents"; then
  echo "$pane" >&2
  echo "Ctrl+O did not expose running child agents" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "↑ parent" && echo "$pane" | grep -Eq "child[[:space:]]+running" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Eq "child[[:space:]]+running"; then
  echo "$pane" >&2
  echo "Running child pane had no live lifecycle indicator in the band" >&2
  exit 1
fi
if echo "$pane" | grep -q "Ask anything"; then
  echo "$pane" >&2
  echo "Running child pane unexpectedly rendered a composer" >&2
  exit 1
fi
tmux send-keys -l -t "$session":0.0 'CHILD_VIEW_MUST_NOT_ACCEPT_INPUT'
tmux send-keys -t "$session":0.0 Enter
child_paste_sequence="$(printf '\033[200~CHILD_VIEW_MUST_NOT_ACCEPT_PASTE\033[201~')"
tmux send-keys -l -t "$session":0.0 "$child_paste_sequence"
sleep 0.2
pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
if echo "$pane" | grep -Eq "CHILD_VIEW_MUST_NOT_ACCEPT_(INPUT|PASTE)"; then
  echo "$pane" >&2
  echo "Running child pane accepted input or paste despite being read-only" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Up
attempt=0
while [ "$attempt" -lt 40 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  ! echo "$pane" | grep -q "↑ parent" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if echo "$pane" | grep -q "↑ parent"; then
  echo "$pane" >&2
  echo "Up did not return from the running child pane" >&2
  exit 1
fi
# The band stays quiet about stopping until asked: one ctrl+c arms the
# interrupt and only then explains the second press.
if echo "$pane" | grep -q "ctrl+c twice"; then
  echo "$pane" >&2
  echo "Status line nags about interrupting before anyone pressed ctrl+c" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 C-c
attempt=0
while [ "$attempt" -lt 30 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "ctrl+c again to interrupt" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "ctrl+c again to interrupt"; then
  echo "$pane" >&2
  echo "First ctrl+c did not arm and explain the interrupt" >&2
  exit 1
fi
# Typing mid-turn must queue and steer, not vanish.
tmux send-keys -l -t "$session":0.0 'steer me please'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 60 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eiq "[[:space:]]steering([[:space:]]|$)" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Eiq "[[:space:]]steering([[:space:]]|$)"; then
  echo "$pane" >&2
  echo "Mid-turn input did not expose its steering delivery status" >&2
  exit 1
fi
# The turn completes: children ran in parallel (well under 2x their sleep),
# and the steering message reached the model (the echo provider replies with
# the wrapped reminder text).
attempt=0
while [ "$attempt" -lt 200 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "steer me please" && echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Ready"; then
  echo "$pane" >&2
  echo "Parallel-children turn did not complete" >&2
  exit 1
fi
if echo "$pane" | grep -Eiq "[[:space:]]steering([[:space:]]|$)"; then
  echo "$pane" >&2
  echo "Consumed steering input retained its queued delivery status" >&2
  exit 1
fi
# Completed child agents remain discoverable/reopenable; finishing the parent
# turn must not erase their task-to-thread navigation links.
tmux send-keys -t "$session":0.0 "/tasks" Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "child agents" && echo "$pane" | grep -q "completed" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "completed"; then
  echo "$pane" >&2
  echo "Completed child agents disappeared from navigation" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 60 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "↑ parent" && echo "$pane" | grep -Eq "child[[:space:]]+completed" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "↑ parent" || ! echo "$pane" | grep -Eq "child[[:space:]]+completed" || echo "$pane" | grep -Eq "child[[:space:]]+running"; then
  echo "$pane" >&2
  echo "Completed child pane did not expose its frozen terminal lifecycle" >&2
  exit 1
fi
if echo "$pane" | grep -q "Ask anything"; then
  echo "$pane" >&2
  echo "Completed child pane unexpectedly rendered a composer" >&2
  exit 1
fi
# Escape inside a child is deliberately non-destructive and no longer means
# "go back".
tmux send-keys -t "$session":0.0 Escape
sleep 0.2
pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
if ! echo "$pane" | grep -q "↑ parent"; then
  echo "$pane" >&2
  echo "Escape unexpectedly left the child-agent pane" >&2
  exit 1
fi

# The visible child navigation is a real mouse surface: move to a sibling,
# then click the parent target to return to the conversation.
child_header_before="$(echo "$pane" | awk '/↑ parent/ { print; exit }')"
next_row="$(echo "$pane" | awk '/next →/ { print NR; exit }')"
next_line="$(echo "$pane" | awk '/next →/ { print; exit }')"
next_col="$(cell_column "$next_line" "next")"
if [ -z "$next_row" ] || [ -z "$next_col" ]; then
  echo "$pane" >&2
  echo "Sibling child-agent navigation was not rendered" >&2
  exit 1
fi
mouse_down="$(printf '\033[<0;%s;%sM' "$next_col" "$next_row")"
mouse_up="$(printf '\033[<0;%s;%sm' "$next_col" "$next_row")"
tmux send-keys -l -t "$session":0.0 "$mouse_down"
tmux send-keys -l -t "$session":0.0 "$mouse_up"
attempt=0
while [ "$attempt" -lt 60 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  child_header_after="$(echo "$pane" | awk '/↑ parent/ { print; exit }')"
  [ -n "$child_header_after" ] && [ "$child_header_after" != "$child_header_before" ] && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ -z "$child_header_after" ] || [ "$child_header_after" = "$child_header_before" ]; then
  echo "$pane" >&2
  echo "Clicking next did not navigate to the sibling child agent" >&2
  exit 1
fi
parent_row="$(echo "$pane" | awk '/↑ parent/ { print NR; exit }')"
parent_line="$(echo "$pane" | awk '/↑ parent/ { print; exit }')"
parent_col="$(cell_column "$parent_line" "parent")"
mouse_down="$(printf '\033[<0;%s;%sM' "$parent_col" "$parent_row")"
mouse_up="$(printf '\033[<0;%s;%sm' "$parent_col" "$parent_row")"
tmux send-keys -l -t "$session":0.0 "$mouse_down"
tmux send-keys -l -t "$session":0.0 "$mouse_up"
attempt=0
while [ "$attempt" -lt 60 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  ! echo "$pane" | grep -q "↑ parent" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if echo "$pane" | grep -q "↑ parent"; then
  echo "$pane" >&2
  echo "Clicking parent did not return from the child-agent pane" >&2
  exit 1
fi
# ---------------------------------------------------------------------------
# P3: Claude-format custom commands and settings rules, honored natively.
# ---------------------------------------------------------------------------
# `/plugin` is a real management surface, and contributed commands use the
# official Claude namespace.
tmux send-keys -t "$session":0.0 C-u
tmux send-keys -l -t "$session":0.0 '/plugin'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Demo Claude plugin" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Demo Claude plugin"; then
  echo "$pane" >&2
  echo "/plugin did not list the installed Claude-format plugin" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape
tmux send-keys -t "$session":0.0 C-u
tmux send-keys -l -t "$session":0.0 '/demo:shipit safely'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Plugin shipped safely" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Plugin shipped safely"; then
  echo "$pane" >&2
  echo "Namespaced Claude plugin command did not expand" >&2
  exit 1
fi

# A terminal file drop arrives as bracketed paste. It must become an image
# chip instead of an unknown `/path` command, and the provider must receive it.
drop_sequence="$(printf '\033[200~%s\033[201~' "$test_root/ws/dropped image.png")"
tmux send-keys -l -t "$session":0.0 "$drop_sequence"
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq "img[[:space:]]+dropped image.png" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Eq "img[[:space:]]+dropped image.png"; then
  echo "$pane" >&2
  echo "Bracketed file drop did not become an image attachment" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "\[attachments: 1\]" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "\[attachments: 1\]"; then
  echo "$pane" >&2
  echo "Dropped image did not reach the provider" >&2
  exit 1
fi

# PDF drops use the same real composer -> API -> runtime -> provider path, but
# render a document-specific chip instead of pretending every file is an image.
pdf_drop_sequence="$(printf '\033[200~%s\033[201~' "$test_root/ws/dropped report.pdf")"
tmux send-keys -l -t "$session":0.0 "$pdf_drop_sequence"
attempt=0
while [ "$attempt" -lt 50 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq "pdf[[:space:]]+dropped report.pdf" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Eq "pdf[[:space:]]+dropped report.pdf"; then
  echo "$pane" >&2
  echo "Bracketed PDF drop did not become a document attachment" >&2
  exit 1
fi
tmux send-keys -l -t "$session":0.0 'inspect pdf'
tmux send-keys -t "$session":0.0 Enter
# Wait for the turn to end, not only for its answer: a command typed while
# the turn is still closing is refused, and the next stanza types one.
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Echo: inspect pdf" && echo "$pane" | grep -q "\[attachments: 1\]" \
    && echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Echo: inspect pdf"; then
  echo "$pane" >&2
  echo "Dropped PDF did not reach the provider" >&2
  exit 1
fi
if [ "${HAARNESS_TMUX_MEDIA_ONLY:-0}" = "1" ]; then
  echo "Haarness tmux media e2e passed"
  exit 0
fi

tmux send-keys -t "$session":0.0 C-u
tmux send-keys -l -t "$session":0.0 '/hello world'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Say hello to world with enthusiasm" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Say hello to world with enthusiasm"; then
  echo "$pane" >&2
  echo "Haarness project command did not expand" >&2
  exit 1
fi
# The Haarness permission rule blocks rm without prompting.
tmux send-keys -t "$session":0.0 C-u
tmux send-keys -l -t "$session":0.0 ' !tool bash {"command":"rm -rf /tmp/haarness-e2e-shouldnotexist"}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "configured policy" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "configured policy"; then
  echo "$pane" >&2
  echo "Haarness permission deny rule was not enforced" >&2
  exit 1
fi

# A denied tool result can render before the turn-completed event. Ctrl+C
# still interrupts while active, so wait for idle before asserting app exit.
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | tail -n 1 | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | tail -n 1 | grep -q "Ready"; then
  echo "$pane" >&2
  echo "Denied command did not return to idle" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 C-c
sleep 0.2
tmux send-keys -t "$session":0.0 C-c
attempt=0
while tmux has-session -t "$session" 2>/dev/null && [ "$attempt" -lt 50 ]; do
  attempt=$((attempt + 1))
  sleep 0.1
done
if tmux has-session -t "$session" 2>/dev/null; then
  echo "TUI did not exit after double Ctrl+C" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Launch contract: a plain reopen creates a fresh home session. Explicit
# --continue restores the last meaningful root conversation, never a child.
# ---------------------------------------------------------------------------
tmux new-session -d -x 100 -y 30 -s "$session" "$pane_env HAARNESS_DATA_DIR='$test_root' HAARNESS_TEST_PROVIDER=1 HAARNESS_DEFAULT_PROVIDER=echo HAARNESS_DEFAULT_MODEL=echo-1 '$binary' --workspace '$test_root/ws'"
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "h a a r n e s s" && echo "$pane" | grep -Eq "v[0-9]+\.[0-9]+\.[0-9]+" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "h a a r n e s s" || ! echo "$pane" | grep -Eq "v[0-9]+\.[0-9]+\.[0-9]+"; then
  echo "$pane" >&2
  echo "Plain reopening did not render a fresh home session" >&2
  exit 1
fi
if echo "$pane" | grep -q "Say hello to world with enthusiasm"; then
  echo "$pane" >&2
  echo "Plain reopening unexpectedly restored the previous transcript" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 C-c C-c
sleep 0.5

tmux new-session -d -x 100 -y 30 -s "$session" "$pane_env HAARNESS_DATA_DIR='$test_root' HAARNESS_TEST_PROVIDER=1 HAARNESS_DEFAULT_PROVIDER=echo HAARNESS_DEFAULT_MODEL=echo-1 '$binary' --continue --workspace '$test_root/ws'"
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Say hello to world with enthusiasm" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "Say hello to world with enthusiasm"; then
  echo "$pane" >&2
  echo "--continue did not restore the previous session transcript" >&2
  exit 1
fi
if echo "$pane" | grep -q "Subagent task"; then
  echo "$pane" >&2
  echo "--continue landed on a child-agent thread" >&2
  exit 1
fi

# Switching to /new is a view change, not cancellation. Start a slow turn,
# switch while its stream is live, and require the new home to receive the
# background completion notification.
tmux send-keys -l -t "$session":0.0 '::mdslow'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 30 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Live markdown" && ! echo "$pane" | grep -q "Ready" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
if ! echo "$pane" | grep -q "Live markdown" || echo "$pane" | grep -q "Ready"; then
  echo "$pane" >&2
  echo "Could not establish a live turn before /new lifecycle testing" >&2
  exit 1
fi
tmux send-keys -l -t "$session":0.0 '/new'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 80 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "h a a r n e s s" && echo "$pane" | grep -q "Haarness finished:" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
if ! echo "$pane" | grep -q "h a a r n e s s" || ! echo "$pane" | grep -q "Haarness finished:"; then
  echo "$pane" >&2
  echo "/new hid the detached turn's background completion" >&2
  exit 1
fi

# Haarness app-exit parity: Ctrl+D exits once from an empty composer even while
# a task is streaming. It must not be swallowed as delete-forward, and the
# persistent server must finish the turn so --continue can reattach later.
tmux send-keys -l -t "$session":0.0 '::mdslow'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 30 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "Live markdown" && break
  attempt=$((attempt + 1))
  sleep 0.05
done
tmux send-keys -t "$session":0.0 C-d
sleep 0.3
if tmux has-session -t "$session" 2>/dev/null; then
  echo "TUI did not exit after one Ctrl+D during an active task" >&2
  exit 1
fi

# No client remains attached here. Reopen only after the slow fixture's full
# duration and prove both the durable tail and an idle composer are restored.
sleep 1.8
tmux new-session -d -x 100 -y 30 -s "$session" "$pane_env HAARNESS_DATA_DIR='$test_root' HAARNESS_TEST_PROVIDER=1 HAARNESS_DEFAULT_PROVIDER=echo HAARNESS_DEFAULT_MODEL=echo-1 '$binary' --continue --workspace '$test_root/ws'"
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q "streams while working" \
    && echo "$pane" | grep -Eq "Ready|Connected" \
    && ! echo "$pane" | grep -q "thinking…" \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q "streams while working" \
  || ! echo "$pane" | grep -Eq "Ready|Connected" \
  || echo "$pane" | grep -q "thinking…"; then
  echo "$pane" >&2
  echo "Detached Ctrl+D turn did not finish on the server and resume cleanly" >&2
  exit 1
fi

# Provider-facing configuration managers execute through the real TUI/model
# loop. Their action surface is local/global administration only; this smoke
# proves configuration plus command and memory resources persist in one
# provider round. Memory deliberately lives outside the Git workspace.
tmux send-keys -l -t "$session":0.0 ' !multi [{"name":"skill_manage","arguments":{"action":"create","name":"tmux-managed-skill","scope":"local","description":"Tmux managed skill","instructions":"Return TMUX_MANAGED_SKILL."}},{"name":"agent_manage","arguments":{"action":"create","name":"tmux-managed-agent","scope":"local","description":"Tmux managed agent","prompt":"Return TMUX_MANAGED_AGENT.","mode":"subagent"}},{"name":"plugin_manage","arguments":{"action":"create","name":"tmux-managed-plugin","scope":"local","description":"Tmux managed plugin"}},{"name":"mcp_manage","arguments":{"action":"add","name":"tmux-managed-mcp","scope":"local","type":"local","command":"not-started","enabled":false}},{"name":"command_manage","arguments":{"action":"create","name":"tmux-managed-command","scope":"local","description":"Tmux managed command","template":"TMUX_COMMAND $ARGUMENTS"}},{"name":"memory_save","arguments":{"name":"tmux-managed-memory","type":"project","description":"Tmux durable memory","content":"TMUX_MEMORY_CONTENT"}}]'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 120 ]; do
  if [ -f "$test_root/ws/.haarness/skills/tmux-managed-skill/SKILL.md" ] \
    && [ -f "$test_root/ws/.haarness/agents/tmux-managed-agent.md" ] \
    && [ -f "$test_root/ws/.haarness/plugins/local/tmux-managed-plugin/.claude-plugin/plugin.json" ] \
    && [ -f "$test_root/ws/.haarness/commands/tmux-managed-command.md" ] \
    && find "$test_root/data/memory" -name 'tmux-managed-memory.md' -type f 2>/dev/null | grep -q . \
    && grep -q 'tmux-managed-mcp' "$test_root/ws/haarness.json" 2>/dev/null; then
    break
  fi
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ ! -f "$test_root/ws/.haarness/skills/tmux-managed-skill/SKILL.md" ] \
  || [ ! -f "$test_root/ws/.haarness/agents/tmux-managed-agent.md" ] \
  || [ ! -f "$test_root/ws/.haarness/plugins/local/tmux-managed-plugin/.claude-plugin/plugin.json" ] \
  || [ ! -f "$test_root/ws/.haarness/commands/tmux-managed-command.md" ] \
  || ! find "$test_root/data/memory" -name 'tmux-managed-memory.md' -type f 2>/dev/null | grep -q . \
  || ! grep -q 'tmux-managed-mcp' "$test_root/ws/haarness.json" 2>/dev/null; then
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" >&2
  echo "Model-facing management tools did not persist every resource type" >&2
  exit 1
fi

# command_manage refreshes the authoritative server registry immediately: a
# connected TUI receives the asset update, and a second client sees the same
# shared server without restarting either process.
# File writes precede turn completion and the assets.updated event. Wait for
# the connected client's live command palette before issuing the new command.
managed_catalog_runtime=$(workspace_runtime_path "$test_root" "$test_root/ws")
managed_catalog_url="$(jq -r '.url' "$managed_catalog_runtime")"
managed_catalog_token="$(jq -r '.token' "$managed_catalog_runtime")"
attempt=0
while [ "$attempt" -lt 100 ]; do
  if curl -sf -H "Authorization: Bearer $managed_catalog_token" "$managed_catalog_url/v1/turns/active" | jq -e 'length == 0' >/dev/null; then
    break
  fi
  attempt=$((attempt + 1))
  sleep 0.1
done
tmux send-keys -t "$session":0.0 C-p
tmux send-keys -l -t "$session":0.0 'tmux-managed-command'
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q '/tmux-managed-command' && echo "$pane" | grep -q 'Tmux managed command' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q '/tmux-managed-command'; then
  echo "$pane" >&2
  echo "Connected TUI did not refresh its command palette" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'Commands' || break
  attempt=$((attempt + 1))
  sleep 0.1
done
tmux send-keys -l -t "$session":0.0 '/tmux-managed-command world'
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q '/tmux-managed-command world' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'TMUX_COMMAND world' && echo "$pane" | grep -q 'Ready' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'TMUX_COMMAND world'; then
  echo "$pane" >&2
  echo "Connected TUI did not refresh the managed command catalog" >&2
  exit 1
fi
command_output="$(env HAARNESS_DATA_DIR="$test_root" HAARNESS_TEST_PROVIDER=1 \
  "$binary" --workspace "$test_root/ws" run \
  --provider echo --model echo-1 --command tmux-managed-command world)"
if ! echo "$command_output" | grep -q 'TMUX_COMMAND world'; then
  echo "$command_output" >&2
  echo "Managed command was not live on the existing server" >&2
  exit 1
fi

tmux send-keys -l -t "$session":0.0 ' !multi [{"name":"memory_list","arguments":{}},{"name":"memory_read","arguments":{"name":"tmux-managed-memory"}},{"name":"memory_search","arguments":{"query":"TMUX_MEMORY_CONTENT"}}]'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'All tools finished' && echo "$pane" | grep -q 'Ready' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'All tools finished'; then
  echo "$pane" >&2
  echo "Durable memory list/read/search tools did not complete" >&2
  exit 1
fi

tmux send-keys -l -t "$session":0.0 ' !multi [{"name":"skill_manage","arguments":{"action":"delete","name":"tmux-managed-skill","scope":"local"}},{"name":"agent_manage","arguments":{"action":"delete","name":"tmux-managed-agent","scope":"local"}},{"name":"plugin_manage","arguments":{"action":"delete","name":"tmux-managed-plugin","scope":"local"}},{"name":"mcp_manage","arguments":{"action":"remove","name":"tmux-managed-mcp","scope":"local"}},{"name":"command_manage","arguments":{"action":"delete","name":"tmux-managed-command","scope":"local"}},{"name":"memory_delete","arguments":{"name":"tmux-managed-memory"}}]'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 120 ]; do
  if [ ! -e "$test_root/ws/.haarness/skills/tmux-managed-skill" ] \
    && [ ! -e "$test_root/ws/.haarness/agents/tmux-managed-agent.md" ] \
    && [ ! -e "$test_root/ws/.haarness/plugins/local/tmux-managed-plugin" ] \
    && [ ! -e "$test_root/ws/.haarness/commands/tmux-managed-command.md" ] \
    && ! find "$test_root/data/memory" -name 'tmux-managed-memory.md' -type f 2>/dev/null | grep -q . \
    && ! grep -q 'tmux-managed-mcp' "$test_root/ws/haarness.json" 2>/dev/null; then
    break
  fi
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ -e "$test_root/ws/.haarness/skills/tmux-managed-skill" ] \
  || [ -e "$test_root/ws/.haarness/agents/tmux-managed-agent.md" ] \
  || [ -e "$test_root/ws/.haarness/plugins/local/tmux-managed-plugin" ] \
  || [ -e "$test_root/ws/.haarness/commands/tmux-managed-command.md" ] \
  || find "$test_root/data/memory" -name 'tmux-managed-memory.md' -type f 2>/dev/null | grep -q . \
  || grep -q 'tmux-managed-mcp' "$test_root/ws/haarness.json" 2>/dev/null; then
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" >&2
  echo "Model-facing management tools did not remove every resource type" >&2
  exit 1
fi

# Empty machine results should read as states, not raw JSON debris. The row is
# intentionally not expandable because there is no detail behind it.
tmux send-keys -l -t "$session":0.0 ' !tool mcp_manage {"action":"list","scope":"local"}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq 'mcp_manage.*no results' \
    && echo "$pane" | grep -q 'Tool mcp_manage =>' \
    && echo "$pane" | grep -q "Ready" \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Eq 'mcp_manage.*no results'; then
  echo "$pane" >&2
  echo "Empty manager output did not render as a semantic state" >&2
  exit 1
fi
if echo "$pane" | grep -Eq '^[[:space:]]*▏[[:space:]]*\[\][[:space:]]*$'; then
  echo "$pane" >&2
  echo "Empty manager output leaked a raw JSON result block" >&2
  exit 1
fi

# Lifecycle parity runs through the same attached TUI and shared server. Give
# the source a unique durable name, create a second session, then exercise
# inspect/message/wait, targeted child abort, and approved agent handoff.
tmux send-keys -l -t "$session":0.0 ' !tool session_rename {"title":"Tmux parity source"}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  sessions_json="$(env HAARNESS_DATA_DIR="$test_root" "$binary" --workspace "$test_root/ws" sessions list --json)"
  source_id="$(echo "$sessions_json" | jq -r '.[] | select(.title == "Tmux parity source") | .id' | head -1)"
  [ -n "$source_id" ] && [ "$source_id" != null ] && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ -z "${source_id:-}" ] || [ "$source_id" = null ]; then
  echo "session_rename did not update the durable source session" >&2
  exit 1
fi

env HAARNESS_DATA_DIR="$test_root" HAARNESS_TEST_PROVIDER=1 \
  "$binary" --workspace "$test_root/ws" run \
  --provider echo --model echo-1 --title 'Tmux parity target' 'target ready' >/dev/null
sessions_json="$(env HAARNESS_DATA_DIR="$test_root" "$binary" --workspace "$test_root/ws" sessions list --json)"
target_id="$(echo "$sessions_json" | jq -r '.[] | select(.title == "Tmux parity target") | .id' | head -1)"
if [ -z "$target_id" ] || [ "$target_id" = null ]; then
  echo "Could not resolve lifecycle target session" >&2
  exit 1
fi

tmux send-keys -l -t "$session":0.0 " !tool session_message {\"threadId\":\"$target_id\",\"message\":\"SESSION_MESSAGE_SENT\",\"wait\":true}"
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 120 ]; do
  target_transcript="$(env HAARNESS_DATA_DIR="$test_root" "$binary" --workspace "$test_root/ws" sessions get "$target_id" --transcript)"
  echo "$target_transcript" | grep -q 'SESSION_MESSAGE_SENT' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$target_transcript" | grep -q 'SESSION_MESSAGE_SENT'; then
  echo "$target_transcript" >&2
  echo "session_message did not reach the target session" >&2
  exit 1
fi

tmux send-keys -l -t "$session":0.0 " !multi [{\"name\":\"session_wait\",\"arguments\":{\"threadId\":\"$target_id\"}},{\"name\":\"session_manage\",\"arguments\":{\"action\":\"digest\",\"threadId\":\"$target_id\"}}]"
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'All tools finished' && echo "$pane" | grep -q 'Ready' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'All tools finished'; then
  echo "$pane" >&2
  echo "session_wait/session_manage did not complete through the model loop" >&2
  exit 1
fi

tmux send-keys -l -t "$session":0.0 ' !tool task {"prompt":"!tool bash {\"command\":\"sleep 10\"}","agent":"general","background":true,"description":"tmux abort target"}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  sessions_json="$(env HAARNESS_DATA_DIR="$test_root" "$binary" --workspace "$test_root/ws" sessions list --json)"
  child_id="$(echo "$sessions_json" | jq -r --arg parent "$source_id" '[.[] | select(.parentId == $parent)] | sort_by(.updatedAt) | last | .id // empty')"
  [ -n "$child_id" ] && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ -z "${child_id:-}" ]; then
  echo "Could not resolve background child for task_abort" >&2
  exit 1
fi
tmux send-keys -l -t "$session":0.0 " !tool task_abort {\"taskId\":\"$child_id\"}"
tmux send-keys -t "$session":0.0 Enter
runtime=$(workspace_runtime_path "$test_root" "$test_root/ws")
runtime_url="$(jq -r '.url' "$runtime")"
runtime_token="$(jq -r '.token' "$runtime")"
attempt=0
while [ "$attempt" -lt 100 ]; do
  background_count="$(curl -sf -H "Authorization: Bearer $runtime_token" "$runtime_url/v1/threads/$source_id/background" | jq 'length')"
  [ "$background_count" = 0 ] && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ "$background_count" != 0 ]; then
  echo "task_abort left the selected child running" >&2
  exit 1
fi

tmux send-keys -l -t "$session":0.0 ' !tool agent_switch {"agent":"plan","reason":"Verify the tmux handoff"}'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'Switch the active agent' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Switch the active agent'; then
  echo "$pane" >&2
  echo "agent_switch did not require explicit approval" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 y
attempt=0
while [ "$attempt" -lt 150 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Eq 'plan[[:space:]]+echo/(echo-1|Echo 1)' \
    && echo "$pane" | grep -q 'Agent handoff completed' \
    && echo "$pane" | grep -q 'Ready' \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Eq 'plan[[:space:]]+echo/(echo-1|Echo 1)' \
  || ! echo "$pane" | grep -q 'Agent handoff completed'; then
  echo "$pane" >&2
  echo "agent_switch did not continue in a fresh target-agent turn" >&2
  exit 1
fi

# The workspace command is a real modal manager, not a read-only text dump.
# Create a Git worktree through its name and discovered-base picker, follow the
# local switch, return to the primary checkout, then reset and remove the
# stopped worktree through the same UI. This crosses renderer/input, REST,
# registry, Git, daemon discovery, and the CLI's in-process workspace-switch
# loop.
tmux send-keys -l -t "$session":0.0 '/worktree'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 150 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'Projects and worktrees' \
    && echo "$pane" | grep -q 'Create worktree' \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Projects and worktrees'; then
  echo "$pane" >&2
  echo "Workspace modal did not open" >&2
  exit 1
fi

# Activate the create row through crossterm's real SGR mouse path.
workspace_create_row="$(printf '%s\n' "$pane" | awk '/Create worktree/{print NR; exit}')"
workspace_create_line="$(printf '%s\n' "$pane" | sed -n "${workspace_create_row}p")"
workspace_create_col="$(cell_column "$workspace_create_line" 'Create worktree')"
mouse_down="$(printf '\033[<0;%s;%sM' "$workspace_create_col" "$workspace_create_row")"
mouse_up="$(printf '\033[<0;%s;%sm' "$workspace_create_col" "$workspace_create_row")"
tmux send-keys -l -t "$session":0.0 "$mouse_down"
tmux send-keys -l -t "$session":0.0 "$mouse_up"
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'Create worktree · name' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Create worktree · name'; then
  echo "$pane" >&2
  echo "Workspace create-name modal did not open by mouse" >&2
  exit 1
fi
tmux send-keys -l -t "$session":0.0 'tmux-modal-workspace'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'base ref' \
    && echo "$pane" | grep -Fq "Local · $workspace_base_ref" \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Fq "Local · $workspace_base_ref"; then
  echo "$pane" >&2
  echo "Workspace discovered-base picker did not show the non-current local branch" >&2
  exit 1
fi
# Filter to the non-current branch so Enter selects its discovered ref rather
# than the default/current branch.
tmux send-keys -l -t "$session":0.0 "$workspace_base_ref"
tmux send-keys -t "$session":0.0 Enter

attempt=0
while [ "$attempt" -lt 300 ]; do
  if [ -f "$test_root/data/workspaces.json" ]; then
    managed_workspace_path="$(jq -r 'first(.workspaces[] | select(.name == "tmux-modal-workspace")) | .path // empty' "$test_root/data/workspaces.json")"
  fi
  [ -n "$managed_workspace_path" ] && [ -d "$managed_workspace_path" ] && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ -z "$managed_workspace_path" ] || [ ! -d "$managed_workspace_path" ]; then
  echo "$pane" >&2
  echo "Workspace modal did not create a real worktree" >&2
  exit 1
fi
if ! git -C "$test_root/ws" worktree list --porcelain | grep -Fq "worktree $managed_workspace_path"; then
  git -C "$test_root/ws" worktree list --porcelain >&2
  echo "Created worktree is not registered as a Git worktree" >&2
  exit 1
fi
managed_workspace_head="$(git -C "$managed_workspace_path" rev-parse HEAD)"
if [ "$managed_workspace_head" != "$workspace_base_commit" ]; then
  echo "Workspace picker created from the wrong Git base" >&2
  echo "expected $workspace_base_ref at $workspace_base_commit" >&2
  echo "actual HEAD $managed_workspace_head" >&2
  exit 1
fi
managed_runtime="$(workspace_runtime_path "$test_root" "$managed_workspace_path")"
managed_workspace_label=${managed_workspace_path##*/}
attempt=0
while [ "$attempt" -lt 150 ] && [ ! -f "$managed_runtime" ]; do
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ ! -f "$managed_runtime" ]; then
  echo "Created worktree did not start its independent server" >&2
  exit 1
fi

# Keep the full workspace state and the unique server-derived place segment
# visible while synchronizing the foreground-client handoff. Earlier compact
# layout assertions already exercised the fixed 100-column viewport.
tmux resize-window -t "$session":0 -x 160 -y 30
attempt=0
while [ "$attempt" -lt 200 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -Fq "$managed_workspace_label" && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Fq "$managed_workspace_label"; then
  echo "$pane" >&2
  echo "TUI did not render the created workspace identity" >&2
  exit 1
fi

# The runtime record is written before the outer CLI loop has necessarily
# finished tearing down the old terminal reader and rendering the new
# workspace client. Sending keys in that handoff window can split one command
# across the two TUI instances. Repeatedly normalize the composer and request
# the dashboard until it renders alongside the unique workspace place segment
# hydrated from the replacement server. That identity cannot come from the old
# primary client, while the modal description itself is deliberately compact.
workspace_dashboard_ready=0
attempt=0
while [ "$attempt" -lt 40 ]; do
  tmux send-keys -t "$session":0.0 Escape
  tmux send-keys -t "$session":0.0 C-u
  tmux send-keys -l -t "$session":0.0 '/workspace'
  tmux send-keys -t "$session":0.0 Enter
  render_attempt=0
  while [ "$render_attempt" -lt 20 ]; do
    pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
    if echo "$pane" | grep -q 'Projects and worktrees' \
      && echo "$pane" | grep -q 'tmux-modal-workspace' \
      && echo "$pane" | grep -Fq "$managed_workspace_label"; then
      workspace_dashboard_ready=1
      break
    fi
    render_attempt=$((render_attempt + 1))
    sleep 0.1
  done
  [ "$workspace_dashboard_ready" -eq 1 ] && break
  attempt=$((attempt + 1))
done
if [ "$workspace_dashboard_ready" -ne 1 ]; then
  echo "$pane" >&2
  echo "Created worktree dashboard did not render in the replacement client" >&2
  exit 1
fi

# Filter by the ownership label so only the primary checkout remains, then
# open it. The contextual action menu focuses its safe Open action.
tmux send-keys -l -t "$session":0.0 'local checkout'
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'Search  local checkout' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Search  local checkout'; then
  echo "$pane" >&2
  echo "Workspace dashboard did not accept its local-checkout filter" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'Folder · ws' \
    && echo "$pane" | grep -q 'Open folder' \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Folder · ws' \
  || ! echo "$pane" | grep -q 'Open folder'; then
  echo "$pane" >&2
  echo "Primary workspace action menu did not open" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 200 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  if echo "$pane" | grep -Fq 'Projects and worktrees' \
    && echo "$pane" | grep -Fq 'local checkout · current'; then
    break
  fi
  if echo "$pane" | grep -Fq 'Projects and worktrees'; then
    # A dashboard from the old server proves the switch is not complete yet.
    tmux send-keys -t "$session":0.0 Escape
  elif ! echo "$pane" | grep -Fq 'Opening folder' \
    && ! echo "$pane" | grep -Fq 'Folder ·'; then
    tmux send-keys -l -t "$session":0.0 '/workspace'
    tmux send-keys -t "$session":0.0 Enter
  fi
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -Fq 'local checkout · current'; then
  echo "$pane" >&2
  echo "TUI did not finish switching back to the primary workspace" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape

# A switched-away daemon deliberately remains shareable. Stop that exact
# workspace before destructive maintenance, as the API requires.
env HAARNESS_DATA_DIR="$test_root" \
  "$binary" --workspace "$managed_workspace_path" serve --stop >/dev/null

tmux send-keys -l -t "$session":0.0 '/workspace'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'Projects and worktrees' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
tmux send-keys -l -t "$session":0.0 'tmux-modal-workspace'
tmux send-keys -t "$session":0.0 Enter
tmux send-keys -t "$session":0.0 Down Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'Reset clean worktree' \
    && echo "$pane" | grep -q 'commits' \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Reset clean worktree'; then
  echo "$pane" >&2
  echo "Workspace reset confirmation did not open" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Down Enter
attempt=0
while [ "$attempt" -lt 150 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'Reset worktree' \
    && echo "$pane" | grep -q 'tmux-modal-workspace' \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Reset worktree'; then
  echo "$pane" >&2
  echo "Workspace reset did not complete through the modal" >&2
  exit 1
fi

# Reset returns to the refreshed dashboard. Refilter, open actions, move from
# Open past Reset to Remove, then take the clean non-branch-deleting choice.
tmux send-keys -l -t "$session":0.0 'tmux-modal-workspace'
tmux send-keys -t "$session":0.0 Enter
tmux send-keys -t "$session":0.0 Down Down Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'Remove clean worktree' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Remove clean worktree'; then
  echo "$pane" >&2
  echo "Workspace remove confirmation did not open" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Down Enter
attempt=0
while [ "$attempt" -lt 200 ]; do
  registered="$(jq -r '[.workspaces[] | select(.name == "tmux-modal-workspace")] | length' "$test_root/data/workspaces.json")"
  [ "$registered" = 0 ] && [ ! -e "$managed_workspace_path" ] && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ "$registered" != 0 ] || [ -e "$managed_workspace_path" ]; then
  echo "Workspace modal did not remove the managed worktree and registry entry" >&2
  exit 1
fi
if git -C "$test_root/ws" worktree list --porcelain | grep -Fq "worktree $managed_workspace_path"; then
  git -C "$test_root/ws" worktree list --porcelain >&2
  echo "Workspace removal left stale Git worktree metadata" >&2
  exit 1
fi
# The server finishes before the TUI hears back, and Ctrl+C is refused while
# the TUI still counts the removal as running. Quit once it says it is done.
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
  echo "$pane" | grep -q 'Removed worktree' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Removed worktree'; then
  echo "$pane" >&2
  echo "The TUI did not report the finished worktree removal" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Escape

tmux send-keys -t "$session":0.0 C-c
sleep 0.2
tmux send-keys -t "$session":0.0 C-c
# Wait for the exit as the first double Ctrl+C check does: a loaded machine
# can take longer than half a second to tear the TUI down.
attempt=0
while tmux has-session -t "$session" 2>/dev/null && [ "$attempt" -lt 50 ]; do
  attempt=$((attempt + 1))
  sleep 0.1
done
if tmux has-session -t "$session" 2>/dev/null; then
  echo "Resumed TUI did not exit after double Ctrl+C" >&2
  exit 1
fi
