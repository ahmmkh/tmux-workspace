#!/bin/sh
# Install tmux-workspace as a normal user and check that it works.
#   In a fresh Linux container, as root:  sh tests/install/smoke.sh
#       With TMUX_WORKSPACE_DIST, a folder of release archives as scripts/package.sh writes them, the
#       install is the piped one (`curl … | bash`) from that folder; without it, from a copy of the
#       working tree, as a clone.
#   On a machine where install.sh ran:    sh tests/install/smoke.sh --checks [ROOT]
set -eu

log() { printf '== %s\n' "$*"; }
fail() {
    log "$*"
    exit 1
}

checks() {
    root=$1
    export PATH="$HOME/.local/bin:$PATH"
    log "tmux $(tmux -V)"
    tmux -f "$HOME/.tmux.conf" new-session -d -s smoke -x 120 -y 40
    sleep 1
    tmux list-keys -T prefix A | grep -q 'tmux-workspace dash' || fail 'Ctrl+a A is not bound'
    tmux show-options -gqv @agent_state | grep -q bash || fail 'agent-status.conf did not load'
    tmux-workspace doctor >"$HOME/doctor.log" 2>&1 || true
    grep -q 'tmux version' "$HOME/doctor.log" || { cat "$HOME/doctor.log"; exit 1; }
    grep WARN "$HOME/doctor.log" || true

    # The agent hook says nothing and succeeds, whatever it is given.
    for payload in '{"hook_event_name": "PreToolUse", "tool_name": "Bash"}' '{"session_id": "abc"}' 'not json' ''; do
        out=$(printf '%s' "$payload" | tmux-workspace agent-hook claude 2>&1) || fail "agent-hook failed on: $payload"
        [ -z "$out" ] || fail "agent-hook printed: $out"
    done

    # pane-start in a labelled pane gives it an identity and becomes TMUX_WORKSPACE_SHELL.
    pane=$(tmux new-window -d -P -F '#{pane_id}' -t smoke 'sleep 60')
    tmux set-option -p -t "$pane" @workspace_label smoke-pane
    rm -f "$HOME/pane-start.status"
    tmux respawn-pane -k -t "$pane" \
        "TMUX_WORKSPACE_SHELL=/bin/true $HOME/.local/bin/tmux-workspace pane-start; echo \$? >$HOME/pane-start.status; sleep 60"
    n=0
    while [ ! -s "$HOME/pane-start.status" ] && [ "$n" -lt 20 ]; do
        sleep 1
        n=$((n + 1))
    done
    [ "$(cat "$HOME/pane-start.status" 2>/dev/null)" = 0 ] || fail 'pane-start did not become TMUX_WORKSPACE_SHELL'
    [ -n "$(tmux display-message -p -t "$pane" '#{@workspace_id}')" ] || fail 'pane-start gave the pane no identity'

    # Python is optional: dash needs it until it is ported, and says so without it.
    if command -v python3 >/dev/null 2>&1 && python3 -c 'import sys; sys.exit(sys.version_info < (3, 9))'; then
        tmux-workspace dash --once --width 100 --height 20 | tee "$HOME/dash.log" | head -5
        grep -q smoke "$HOME/dash.log" || fail 'the dashboard does not list the smoke workspace'
    else
        if tmux-workspace dash --once >"$HOME/dash.log" 2>&1; then fail 'dash ran without python3'; fi
        grep -q 'needs python3 3.9+' "$HOME/dash.log" || { cat "$HOME/dash.log"; fail 'dash did not say it needs python3'; }
        log 'no python3: dash says it needs it'
    fi

    # Outside tmux, work creates the workspace and then tries to attach, which needs a terminal.
    bash -ic 'work demo-project' </dev/null >/dev/null 2>&1 || true
    tmux has-session -t '=demo-project' || fail 'work did not create the workspace'
    [ -d "$HOME/Projects/demo-project" ] || fail 'work did not create the project folder'

    if [ -n "${TMUX_WORKSPACE_DIST:-}" ]; then
        tmux-workspace update --check | tee "$HOME/update.log"
        grep -q 'already up to date' "$HOME/update.log" || fail 'update --check does not know this release'
    fi

    tmux kill-server
    bash "$root/install.sh" --uninstall
    [ ! -e "$HOME/.tmux.conf" ] || fail 'uninstall left ~/.tmux.conf'
    [ ! -e "$HOME/.local/bin/tmux-workspace" ] || fail 'uninstall left ~/.local/bin/tmux-workspace'
    log PASS
}

as_user() {
    if [ -n "${TMUX_WORKSPACE_DIST:-}" ]; then
        # The script on standard input, as from curl; the release from the dist folder.
        bash -s -- --deps --yes --projects "$HOME/Projects" <"$HOME/tmux-workspace/install.sh"
        checks "$HOME/.local/share/tmux-workspace/current"
    else
        cd "$HOME/tmux-workspace"
        bash install.sh --deps --yes --projects "$HOME/Projects"
        checks "$HOME/tmux-workspace"
    fi
}

case "${1:-}" in
--checks)
    checks "${2:-$(cd "$(dirname "$0")/../.." && pwd -P)}"
    exit 0
    ;;
--as-user)
    as_user
    exit 0
    ;;
esac

# Root in a fresh container: just enough to create a sudo user. install.sh --deps does the rest.
if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq >/dev/null
    apt-get install -y -qq sudo bash ca-certificates >/dev/null
elif command -v dnf >/dev/null 2>&1; then
    dnf install -y -q sudo bash findutils util-linux >/dev/null
elif command -v pacman >/dev/null 2>&1; then
    # pacman's download sandbox cannot start inside a container.
    grep -qx 'DisableSandbox' /etc/pacman.conf || sed -i '/^\[options\]/a DisableSandbox' /etc/pacman.conf
    pacman -Sy --noconfirm --needed sudo bash >/dev/null
elif command -v apk >/dev/null 2>&1; then
    apk add -q sudo bash
fi
if command -v useradd >/dev/null 2>&1; then
    useradd -m -s /bin/bash tester
else
    adduser -D -s /bin/bash tester
fi
mkdir -p /etc/sudoers.d
echo 'tester ALL=(ALL) NOPASSWD: ALL' >/etc/sudoers.d/tester
mkdir -p /home/tester/tmux-workspace
# The working tree, without git internals, anyone's personal config or build output.
tar -C /src --exclude=./.git --exclude=./.superpowers --exclude=./config.json --exclude=./target \
    --exclude=./dist --exclude=./bin/tmux-workspace -cf - . | tar -C /home/tester/tmux-workspace -xf -
dist=''
if [ -n "${TMUX_WORKSPACE_DIST:-}" ]; then
    # A copy the user can read, whatever the modes of the mounted folder.
    dist=/home/tester/dist
    cp -R "$TMUX_WORKSPACE_DIST" "$dist"
fi
chown -R tester /home/tester/tmux-workspace ${dist:+"$dist"}
su - tester -c "TMUX_WORKSPACE_DIST='$dist' sh /home/tester/tmux-workspace/tests/install/smoke.sh --as-user"
