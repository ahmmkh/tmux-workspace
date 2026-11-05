#!/bin/sh
# Time the five paths the Rust port made fast, against the Python they replaced:
#
#   scripts/bench.sh            or: make bench
#
# It builds the release binary, starts a scratch tmux server (its own -L socket, -f /dev/null, a
# temporary state directory and HOME: your tmux server and ~/.local/state are never touched) with five
# panes in two sessions, and times each path with hyperfine (-N -w 3 -r 30). The Python column runs
# legacy/bin/tmux-workspace and legacy/integrations/agent-hook.py the same way, on the same server.
# The table goes to stdout, everything else to stderr. Exit status 1: a Rust row is over its target.
#
# The timing runs inside a pane of the server, in a process named claude (a copy of sh), because
# `agent-hook claude-status` only publishes when a Claude Code process owns the pane: the benchmark
# is that process's child, as Claude Code's own hook calls are. Without that, the hook would stop at
# the owner check and the row would measure a call that does nothing.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd -P)

# The five rows and their targets in ms.
ROWS='hook pane-start claude-status help save'
target_of() {
    case $1 in
        hook) echo 5 ;;
        pane-start) echo 15 ;;
        claude-status) echo 20 ;;
        help) echo 5 ;;
        save) echo 60 ;;
    esac
}
label_of() {
    case $1 in
        hook) echo 'Agent hook, PreToolUse for Bash' ;;
        pane-start) echo 'pane-start, until it execs the shell' ;;
        claude-status) echo 'agent-hook claude-status' ;;
        help) echo 'help' ;;
        save) echo 'save, 5 panes' ;;
    esac
}

# ---------------------------------------------------------------------------------------------
# Inside the scratch server: the process named claude runs this part.

# measure ROW IMPL INPUT COMMAND: hyperfine without a shell; the result is $out/ROW.IMPL.md.
# INPUT is the file that becomes the command's stdin, or null.
measure() {
    hyperfine -N -w 3 -r 30 --style basic --input "$3" \
        -n "$2" --export-markdown "$out/$1.$2.md" "$4" >>"$work/hyperfine.log" 2>&1
}

inside() {
    work=${BENCH_WORK:?}
    trap 'echo $? >"$work/status"' EXIT
    exec >>"$work/inside.log" 2>&1
    out=$work/out
    rust=${BENCH_BIN:?}
    python=${BENCH_PYTHON-}
    legacy=$root/legacy/bin/tmux-workspace
    legacy_hook=$root/legacy/integrations/agent-hook.py

    # The hook payloads. The session id is 18 characters; the status line shows the first 8.
    printf '%s' '{"hook_event_name":"PreToolUse","tool_name":"Bash","session_id":"claude-bench-session","cwd":"/tmp"}' >"$work/pretooluse.json"
    printf '%s' '{"session_id":"claude-bench-session","cwd":"/tmp"}' >"$work/status.json"
    # What Claude Code writes for a running session; the status line turns it into "working".
    mkdir -p "$CLAUDE_CONFIG_DIR/sessions"
    printf '%s' '{"status":"busy"}' >"$CLAUDE_CONFIG_DIR/sessions/$$.json"

    # Sanity checks: each command does the work that is timed, not an early exit. The hook writes to
    # a file: in a command substitution its parent would be a copy of this shell, a second "claude"
    # between it and the pane, and the hook refuses a pane with two.
    "$rust" agent-hook claude <"$work/pretooluse.json" >"$work/sanity"
    [ ! -s "$work/sanity" ]
    "$rust" agent-hook claude-status <"$work/status.json" >"$work/sanity"
    [ "$(cat "$work/sanity")" = 'Claude | claude-b' ] ||
        { echo "bench: the Rust hook found no Claude process to publish for"; exit 1; }
    [ "$(tmux show-options -p -v -t "$TMUX_PANE" @workspace_agent)" = working ] ||
        { echo "bench: the Rust hook did not publish the status"; exit 1; }
    python_status=1
    if [ -n "$python" ]; then
        "$python" "$legacy_hook" claude-status <"$work/status.json" >"$work/sanity" || true
        if [ "$(cat "$work/sanity")" = 'Claude | claude-b' ]; then python_status=0; fi
    fi

    # --- hook
    measure hook rust "$work/pretooluse.json" "\"$rust\" agent-hook claude"
    if [ -n "$python" ]; then
        measure hook python "$work/pretooluse.json" "\"$python\" \"$legacy_hook\" claude"
    fi

    # --- pane-start, in a labelled pane, until it execs the shell (true)
    (
        TMUX_PANE=$BENCH_TARGET TMUX_WORKSPACE_SHELL=/usr/bin/true
        export TMUX_PANE TMUX_WORKSPACE_SHELL
        "$rust" pane-start
        measure pane-start rust null "\"$rust\" pane-start"
        if [ -n "$python" ]; then
            measure pane-start python null "\"$python\" \"$legacy\" pane-start"
        fi
    )

    # --- agent-hook claude-status, from the claude process
    measure claude-status rust "$work/status.json" "\"$rust\" agent-hook claude-status"
    if [ "$python_status" = 0 ]; then
        measure claude-status python "$work/status.json" "\"$python\" \"$legacy_hook\" claude-status"
    fi

    # --- help (not a terminal, so it prints every topic)
    measure help rust null "\"$rust\" help"
    if [ -n "$python" ]; then
        measure help python null "\"$python\" \"$legacy\" help"
    fi

    # --- save: each one has a state directory of its own, so neither finds the other's snapshot
    (
        TMUX_WORKSPACE_STATE_DIR=$work/state-rust
        export TMUX_WORKSPACE_STATE_DIR
        "$rust" save
        measure save rust null "\"$rust\" save"
    )
    if [ -n "$python" ]; then
        (
            TMUX_WORKSPACE_STATE_DIR=$work/state-python
            export TMUX_WORKSPACE_STATE_DIR
            "$python" "$legacy" save
            measure save python null "\"$python\" \"$legacy\" save"
        )
    fi
    exit 0
}

if [ "${1-}" = --inside ]; then
    inside
fi

# ---------------------------------------------------------------------------------------------
# Outside: build, set the scene, wait for the run inside, print the table.

if [ $# -ne 0 ]; then
    echo 'usage: scripts/bench.sh' >&2
    exit 2
fi
for tool in hyperfine tmux cargo; do
    command -v "$tool" >/dev/null 2>&1 || { echo "bench: $tool is needed" >&2; exit 1; }
done
# A tmux server of yours must not catch the scratch commands.
unset TMUX TMUX_PANE

python=
if command -v python3 >/dev/null 2>&1; then python=$(command -v python3); fi

echo 'bench: building the release binary' >&2
(cd "$root" && cargo build --release --locked) >&2
rust=${CARGO_TARGET_DIR:-$root/target}/release/tmux-workspace
[ -x "$rust" ] || { echo "bench: no binary at $rust" >&2; exit 1; }

work=$(mktemp -d)
socket=tw-bench-$$
tm() { tmux -u -L "$socket" -f /dev/null "$@"; }
socket_file=
cleanup() {
    tm kill-server >/dev/null 2>&1 || true
    # tmux can leave the socket of a server that was killed.
    if [ -n "$socket_file" ]; then rm -f "$socket_file"; fi
    rm -rf "${work:?}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
mkdir "$work/out" "$work/bin" "$work/home" "$work/claude" "$work/state-rust" "$work/state-python"

# A process named claude: a copy of sh (a link would show up as sh). macOS kills a copy of one of
# its own programs until it is signed again.
cp "$(command -v sh)" "$work/bin/claude"
if command -v codesign >/dev/null 2>&1; then codesign --force --sign - "$work/bin/claude" >/dev/null 2>&1 || true; fi
"$work/bin/claude" -c 'exit 0' 2>/dev/null || { echo 'bench: could not make a process named claude' >&2; exit 1; }

# Two sessions, five panes: alpha has two tabs (two panes and one), beta a tab and the agent tab.
quiet='exec sleep 600'
tm new-session -d -s alpha -n one -x 120 -y 40 "$quiet"
socket_file=$(tm display-message -p '#{socket_path}')
target=$(tm display-message -t alpha:one -p '#{pane_id}')
tm split-window -d -t alpha:one "$quiet"
tm new-window -d -t alpha: -n two "$quiet"
tm new-session -d -s beta -n code -x 120 -y 40 "$quiet"
# The pane that pane-start is timed in has its label, so it never asks for one.
tm set-option -p -t "$target" @workspace_label main
tm new-window -d -t beta: -n agent \
    -e "BENCH_WORK=$work" -e "BENCH_BIN=$rust" -e "BENCH_PYTHON=$python" -e "BENCH_TARGET=$target" \
    -e "HOME=$work/home" -e "CLAUDE_CONFIG_DIR=$work/claude" -e "TMUX_WORKSPACE_STATE_DIR=$work/state-rust" \
    "$work/bin/claude" "$root/scripts/bench.sh" --inside
panes=$(tm list-panes -a | wc -l | tr -d ' ')
[ "$panes" -eq 5 ] || { echo "bench: expected 5 panes, found $panes" >&2; exit 1; }

echo 'bench: timing (about a minute)' >&2
waited=0
while [ ! -e "$work/status" ]; do
    # The agent tab closes when the run ends, and the run writes its status first.
    if ! tm list-windows -t beta: -F '#{window_name}' | grep -qx agent && [ ! -e "$work/status" ]; then
        echo 'bench: the run in the scratch server ended without a result' >&2
        cat "$work/inside.log" "$work/hyperfine.log" >&2 2>/dev/null || true
        exit 1
    fi
    sleep 1
    waited=$((waited + 1))
    if [ "$waited" -gt 600 ]; then echo 'bench: gave up after 10 minutes' >&2; exit 1; fi
done
if [ "$(cat "$work/status")" != 0 ]; then
    cat "$work/inside.log" "$work/hyperfine.log" >&2 2>/dev/null || true
    echo "bench: the run in the scratch server failed (status $(cat "$work/status"))" >&2
    exit 1
fi

# --- the table
tmux_version=$(tmux -V)
case $(uname -s) in
    Darwin)
        cpu=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || true)
        system="macOS $(sw_vers -productVersion 2>/dev/null || true)"
        ;;
    *)
        cpu=$(sed -n 's/^model name[[:space:]]*:[[:space:]]*//p' /proc/cpuinfo 2>/dev/null | head -n 1)
        system=$(uname -sr)
        ;;
esac
cores=$(getconf _NPROCESSORS_ONLN 2>/dev/null || true)
python_version=none
if [ -n "$python" ]; then python_version=$("$python" -c 'import platform; print(platform.python_version())'); fi
revision=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
version=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n 1)
printf 'tmux-workspace %s (%s), release build, on %s, %s (%s cores), %s, Python %s, hyperfine %s.\n' \
    "$version" "$revision" "${cpu:-$(uname -m)}" "$system" "${cores:-?}" "$tmux_version" "$python_version" \
    "$(hyperfine --version | sed 's/^hyperfine //')"
printf 'Method: hyperfine -N -w 3 -r 30, a scratch tmux server with 5 panes in 2 sessions, mean ± standard deviation.\n\n'
printf '| Path | Target | Rust | Python |\n|---|---|---|---|\n'

# cell ROW IMPL: the "mean ± deviation" in ms that hyperfine measured, or nothing. hyperfine picks the
# unit of its table by itself and names it in the header ("Mean [ms]").
cell() {
    [ -f "$work/out/$1.$2.md" ] || return 0
    awk -F'|' '
        NR == 1 { unit = $3; sub(/.*\[/, "", unit); sub(/\].*/, "", unit) }
        NR == 3 {
            scale = (unit == "s") ? 1000 : (unit == "ms") ? 1 : 0.001
            split($3, parts, "±")
            printf "%.1f ± %.1f\n", parts[1] * scale, parts[2] * scale
        }
    ' "$work/out/$1.$2.md"
}
over=''
for row in $ROWS; do
    target_ms=$(target_of "$row")
    rust_cell=$(cell "$row" rust)
    python_cell=$(cell "$row" python)
    note=''
    if ! awk -v mean="${rust_cell%% *}" -v target="$target_ms" 'BEGIN { exit !(mean + 0 <= target + 0) }'; then
        note=' (over)'
        over="$over $row"
    fi
    python_text='n/a'
    if [ -n "$python_cell" ]; then python_text="$python_cell ms"; fi
    printf '| %s | ≤ %s ms | %s ms%s | %s |\n' "$(label_of "$row")" "$target_ms" "$rust_cell" "$note" "$python_text"
done
if [ -n "$over" ]; then
    echo >&2
    echo "bench: over the target:$over" >&2
    exit 1
fi
