#!/bin/sh
# Install tmux-workspace as a normal user and check that it works.
#   In a fresh Linux container, as root:  sh tests/install/smoke.sh
#   On a machine where install.sh ran:    sh tests/install/smoke.sh --checks
set -eu

log() { printf '== %s\n' "$*"; }

checks() {
    export PATH="$HOME/.local/bin:$PATH"
    log "tmux $(tmux -V)"
    tmux -f "$HOME/.tmux.conf" new-session -d -s smoke -x 120 -y 40
    sleep 1
    tmux list-keys -T prefix A | grep -q 'tmux-workspace dash' || { log 'Ctrl+a A is not bound'; exit 1; }
    tmux show-options -gqv @agent_state | grep -q bash || { log 'agent-status.conf did not load'; exit 1; }
    tmux-workspace doctor >"$HOME/doctor.log" 2>&1 || true
    grep -q 'tmux version' "$HOME/doctor.log" || { cat "$HOME/doctor.log"; exit 1; }
    grep WARN "$HOME/doctor.log" || true
    tmux-workspace dash --once --width 100 --height 20 | tee "$HOME/dash.log" | head -5
    grep -q smoke "$HOME/dash.log" || { log 'dashboard does not list the smoke workspace'; exit 1; }
    # Outside tmux, work creates the workspace and then tries to attach, which needs a terminal.
    bash -ic 'work demo-project' </dev/null >/dev/null 2>&1 || true
    tmux has-session -t '=demo-project' || { log 'work did not create the workspace'; exit 1; }
    [ -d "$HOME/Projects/demo-project" ] || { log 'work did not create the project folder'; exit 1; }
    tmux kill-server
    bash "$1/install.sh" --uninstall
    [ ! -e "$HOME/.tmux.conf" ] || { log 'uninstall left ~/.tmux.conf'; exit 1; }
    log PASS
}

as_user() {
    cd "$HOME/tmux-workspace"
    bash install.sh --deps --yes --projects "$HOME/Projects"
    checks "$HOME/tmux-workspace"
}

case "${1:-}" in
--checks)
    checks "$(cd "$(dirname "$0")/../.." && pwd -P)"
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
# The working tree, without git internals or anyone's personal config.
tar -C /src --exclude=./.git --exclude=./.superpowers --exclude=./config.json -cf - . | tar -C /home/tester/tmux-workspace -xf -
chown -R tester /home/tester/tmux-workspace
su - tester -c 'sh /home/tester/tmux-workspace/tests/install/smoke.sh --as-user'
