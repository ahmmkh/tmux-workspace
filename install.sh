#!/usr/bin/env bash
# tmux-workspace installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/ahmmkh/tmux-workspace/main/install.sh | bash
#   ./install.sh [options]          from a clone; links point into that clone
#
# Every file it replaces moves to ~/.local/state/tmux-workspace/backups/ and --uninstall
# puts it back. Running it twice changes nothing the second time. Bash 3.2 compatible.
set -euo pipefail

TMUX_MIN=3.3
PYTHON_MIN=3.9
REPO_URL=${TMUX_WORKSPACE_REPO:-https://github.com/ahmmkh/tmux-workspace.git}
BLOCK_START='# >>> tmux-workspace >>>'
BLOCK_END='# <<< tmux-workspace <<<'

usage() {
    cat <<'EOF'
Usage: install.sh [options]

  --deps            install missing tools (tmux, python3, fzf, zoxide) with the package manager
  --projects DIR    the folder that holds your projects (otherwise you are asked once)
  --agents          also register Claude Code, Codex, omp and Neovim integrations
  --no-shell        do not add the `work` command to ~/.zshrc, ~/.bashrc or fish
  --no-tmux-conf    do not link ~/.tmux.conf
  --yes, -y         answer yes: replace an existing ~/.tmux.conf (a backup is kept)
  --dry-run         print what would change and change nothing
  --uninstall       remove the links and shell lines, and restore the backups
  -h, --help        show this help

Environment: TMUX_WORKSPACE_HOME (where a piped install clones to), TMUX_WORKSPACE_REPO.
EOF
}

say() { printf '%-8s %s\n' "$1" "$2"; }
die() { printf 'error    %s\n' "$1" >&2; exit "${2:-1}"; }

DEPS=0 AGENTS=0 SHELLS=1 TMUXCONF=1 DRY=0 YES=0 UNINSTALL=0 PROJECTS=''
ORIGINAL_ARGS=("$@")
while [ $# -gt 0 ]; do
    case $1 in
        --deps) DEPS=1 ;;
        --agents) AGENTS=1 ;;
        --projects) [ $# -ge 2 ] || { usage >&2; exit 2; }; PROJECTS=$2; shift ;;
        --projects=*) PROJECTS=${1#*=} ;;
        --no-shell) SHELLS=0 ;;
        --no-tmux-conf) TMUXCONF=0 ;;
        --dry-run) DRY=1 ;;
        --yes|-y) YES=1 ;;
        --uninstall) UNINSTALL=1 ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'unknown option: %s\n\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

HOME_CONFIG=${XDG_CONFIG_HOME:-$HOME/.config}
BIN_DIR=$HOME/.local/bin
STATE=$HOME/.local/state/tmux-workspace
CONFIG_DIR=$HOME_CONFIG/tmux-workspace
STATUS_CONF=$HOME/.config/tmux-workspace/agent-status.conf  # tmux.conf sources this fixed path
CHANGED=0
BACKUP_DIR=''

# ---------------------------------------------------------------------------------------------
# Helpers

# act LABEL TEXT COMMAND... runs COMMAND, or only prints it on a dry run.
act() {
    local label=$1 text=$2
    shift 2
    CHANGED=1
    if [ "$DRY" = 1 ]; then
        say would "$label $text"
    else
        "$@"
        say "$label" "$text"
    fi
}

has() { command -v "$1" >/dev/null 2>&1; }

# resolve PATH prints PATH with every symlink followed; works without readlink -f.
resolve() {
    local path=$1 link n=0 dir
    while [ -L "$path" ] && [ "$n" -lt 40 ]; do
        link=$(readlink "$path")
        case $link in
            /*) path=$link ;;
            *) path=$(dirname "$path")/$link ;;
        esac
        n=$((n + 1))
    done
    if dir=$(cd "$(dirname "$path")" 2>/dev/null && pwd -P); then
        printf '%s/%s\n' "$dir" "$(basename "$path")"
    else
        printf '%s\n' "$path"
    fi
}

# A terminal to ask on, even when the script itself arrives on a pipe.
tty_ok() { { : </dev/tty; } 2>/dev/null; }

ask() {
    local answer
    tty_ok || return 1
    printf '%s [y/N] ' "$1" >/dev/tty
    read -r answer </dev/tty || return 1
    case $answer in y|Y|yes|YES) return 0 ;; *) return 1 ;; esac
}

version_ge() {  # version_ge 3.4 3.3
    local h1=${1%%.*} h2=${1#*.} n1=${2%%.*} n2=${2#*.}
    [ "$h1" -gt "$n1" ] || { [ "$h1" -eq "$n1" ] && [ "$h2" -ge "$n2" ]; }
}

tmux_version() {
    tmux -V 2>/dev/null | sed -n 's/[^0-9]*\([0-9][0-9]*\)\.\([0-9][0-9]*\).*/\1.\2/p'
}

python_version() {
    python3 -c 'import sys; print("%d.%d" % sys.version_info[:2])' 2>/dev/null || true
}

# ---------------------------------------------------------------------------------------------
# Where the code lives: this clone, or a fresh clone when the script arrives through a pipe.

script_dir() {
    local source=${BASH_SOURCE[0]:-}
    [ -n "$source" ] && [ -f "$source" ] || return 1
    (cd "$(dirname "$source")" && pwd -P)
}

PM='' SUDO=''
detect_platform() {
    local pm
    for pm in brew apt-get dnf pacman apk zypper; do
        if has "$pm"; then PM=$pm; break; fi
    done
    if [ "$PM" != brew ] && [ "$(id -u)" != 0 ]; then SUDO=sudo; fi
}

package() {  # the package name of a tool for this package manager
    case "$1:$PM" in
        python3:pacman) echo python ;;
        *) echo "$1" ;;
    esac
}

install_command() {  # install_command TOOL...  prints the command a person would run
    local names='' tool
    for tool in "$@"; do names="$names $(package "$tool")"; done
    case $PM in
        brew) echo "brew install$names" ;;
        apt-get) echo "${SUDO:+$SUDO }apt-get install -y$names" ;;
        dnf) echo "${SUDO:+$SUDO }dnf install -y$names" ;;
        pacman) echo "${SUDO:+$SUDO }pacman -S --needed --noconfirm$names" ;;
        apk) echo "${SUDO:+$SUDO }apk add$names" ;;
        zypper) echo "${SUDO:+$SUDO }zypper install -y$names" ;;
        *) echo "install$names with your package manager" ;;
    esac
}

install_packages() {
    [ -n "$PM" ] || die "no supported package manager found; install$(printf ' %s' "$@") yourself" 3
    if [ -n "$SUDO" ] && ! has sudo; then die "sudo is needed to run: $(install_command "$@")" 3; fi
    if [ "$PM" = apt-get ]; then $SUDO apt-get update -qq; fi
    say install "$*"
    # shellcheck disable=SC2046  # one word per package
    case $PM in
        brew) brew install $(for t in "$@"; do package "$t"; done) ;;
        apt-get) DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq $(for t in "$@"; do package "$t"; done) ;;
        dnf) $SUDO dnf install -y -q $(for t in "$@"; do package "$t"; done) ;;
        pacman) $SUDO pacman -S --needed --noconfirm $(for t in "$@"; do package "$t"; done) ;;
        apk) $SUDO apk add -q $(for t in "$@"; do package "$t"; done) ;;
        zypper) $SUDO zypper install -y $(for t in "$@"; do package "$t"; done) ;;
    esac
}

remote_install() {
    local target=${TMUX_WORKSPACE_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/tmux-workspace}
    detect_platform
    if ! has git; then
        [ "$DEPS" = 1 ] || die "git is needed to download tmux-workspace: $(install_command git)" 3
        install_packages git
    fi
    if [ -d "$target/.git" ]; then
        git -C "$target" pull --ff-only --quiet
        say update "$target"
    else
        mkdir -p "$(dirname "$target")"
        git clone --quiet --depth 1 "$REPO_URL" "$target"
        say clone "$target"
    fi
    exec bash "$target/install.sh" ${ORIGINAL_ARGS[@]+"${ORIGINAL_ARGS[@]}"}
}

# ---------------------------------------------------------------------------------------------
# Requirements

check_requirements() {
    local missing='' old='' tool version
    if has tmux; then
        version=$(tmux_version)
        if [ -n "$version" ] && ! version_ge "$version" "$TMUX_MIN"; then
            old="tmux $version is older than $TMUX_MIN"
        fi
        say ok "tmux $(tmux -V 2>/dev/null | sed 's/^tmux //')"
    else
        missing="$missing tmux"
    fi
    version=$(python_version)
    if [ -z "$version" ]; then
        missing="$missing python3"
    elif ! version_ge "$version" "$PYTHON_MIN"; then
        old="${old:+$old; }python3 $version is older than $PYTHON_MIN"
    else
        say ok "python3 $version"
    fi
    local optional=''
    for tool in fzf zoxide git sesh lazygit; do
        if has "$tool"; then say ok "$tool"; else optional="$optional $tool"; fi
    done

    if [ -n "$missing" ] || [ -n "$old" ]; then
        if [ "$DEPS" = 1 ] && [ "$DRY" = 0 ]; then
            # shellcheck disable=SC2086
            install_packages $missing fzf zoxide
            check_requirements_again
            return
        fi
        [ -z "$old" ] || say old "$old"
        # shellcheck disable=SC2086
        [ -z "$missing" ] || say missing "$(install_command $missing)"
        die "tmux-workspace needs tmux $TMUX_MIN+ and python3 $PYTHON_MIN+; rerun with --deps to install them" 3
    fi
    for tool in $optional; do
        case $tool in
            fzf) say optional "fzf: fuzzy workspace and tab pickers ($(install_command fzf))" ;;
            zoxide) say optional "zoxide: \`work\` finds folders you visit often" ;;
            git) say optional "git: updates with git pull" ;;
            sesh) say optional "sesh: picker fallback without fzf" ;;
            lazygit) say optional "lazygit: Ctrl+a g opens it in a popup" ;;
        esac
    done
}

check_requirements_again() {
    local version
    has tmux || die "tmux is still missing after installing it" 3
    version=$(tmux_version)
    if [ -n "$version" ] && ! version_ge "$version" "$TMUX_MIN"; then
        die "your package manager has tmux $version; tmux-workspace needs $TMUX_MIN or newer" 3
    fi
    version=$(python_version)
    if [ -z "$version" ] || ! version_ge "$version" "$PYTHON_MIN"; then die "python3 $PYTHON_MIN+ is still missing" 3; fi
}

# ---------------------------------------------------------------------------------------------
# Changes, each one backed up first

ensure_backup_dir() {
    [ -z "$BACKUP_DIR" ] || return 0
    BACKUP_DIR=$STATE/backups/install-$(date +%Y%m%d-%H%M%S)-$$
    if [ "$DRY" = 0 ]; then
        mkdir -p "$BACKUP_DIR"
        chmod 700 "$STATE" "$STATE/backups" "$BACKUP_DIR"
    fi
}

backup() {
    local path=$1 kind='file' name
    if [ -L "$path" ]; then kind='link'; fi
    ensure_backup_dir
    name=$(printf '%s' "${path#"$HOME"/}" | tr '/' '%')
    act backup "$path -> $BACKUP_DIR/$name" mv "$path" "$BACKUP_DIR/$name"
    [ "$DRY" = 1 ] || printf '%s\t%s\t%s\n' "$path" "$BACKUP_DIR/$name" "$kind" >>"$BACKUP_DIR/manifest"
}

make_link() { mkdir -p "$(dirname "$2")"; ln -sfn "$1" "$2"; }

ours() { [ -L "$1" ] && [ "$(resolve "$1")" = "$(resolve "$2")" ]; }

link() {  # link SOURCE DEST
    local source=$1 dest=$2
    ours "$dest" "$source" && return 0
    if [ -e "$dest" ] || [ -L "$dest" ]; then backup "$dest"; fi
    act link "$dest -> $source" make_link "$source" "$dest"
}

link_tmux_conf() {
    local dest=$HOME/.tmux.conf
    ours "$dest" "$REPO/tmux.conf" && return 0
    if { [ -e "$dest" ] || [ -L "$dest" ]; } && [ "$YES" = 0 ]; then
        if ! ask "Replace your ~/.tmux.conf? It moves to a backup and --uninstall restores it."; then
            say skip "$dest exists; rerun with --yes to replace it (a backup is kept)"
            return 0
        fi
    fi
    link "$REPO/tmux.conf" "$dest"
}

append_block() {
    printf '\n%s\n[ -f "%s/shell/work.sh" ] && . "%s/shell/work.sh"\n%s\n' \
        "$BLOCK_START" "$REPO" "$REPO" "$BLOCK_END" >>"$1"
}

strip_block() {
    local tmp
    tmp=$(mktemp)
    awk -v start="$BLOCK_START" -v end="$BLOCK_END" \
        '$0 == start { skip = 1 } !skip { print } $0 == end { skip = 0 }' "$1" >"$tmp"
    cat "$tmp" >"$1"  # Write through, so a symlinked rc file stays a symlink.
    rm -f "$tmp"
}

shell_rc() {  # shell_rc RCFILE SHELLNAME
    local rc=$1 shell=$2 target
    if [ ! -e "$rc" ] && [ "$(basename "${SHELL:-}")" != "$shell" ]; then return 0; fi
    target=$(resolve "$rc")
    if [ -f "$target" ] && grep -qF "$BLOCK_START" "$target"; then return 0; fi
    if [ -f "$target" ] && grep -q 'tmux-workspace/shell.zsh' "$target"; then return 0; fi
    act edit "$rc (adds the work command)" append_block "$target"
}

install_config() {
    mkdir -p "$(dirname "$2")"
    chmod 700 "$(dirname "$2")"
    cp "$1" "$2"
    chmod 600 "$2"
}

migrate_config() {
    local dest=$CONFIG_DIR/config.json
    [ -f "$dest" ] && return 0
    if [ -f "$REPO/config.json" ]; then
        act migrate "$REPO/config.json -> $dest" install_config "$REPO/config.json" "$dest"
    else
        act create "$dest" install_config "$REPO/config.example.json" "$dest"
    fi
}

onboard() {
    local tool=$BIN_DIR/tmux-workspace
    if [ -n "$PROJECTS" ]; then
        if [ "$DRY" = 1 ]; then say would "set the projects folder to $PROJECTS"; return 0; fi
        "$tool" setup --projects "$PROJECTS" >/dev/null
        say setup "projects folder $PROJECTS"
        CHANGED=1
    elif grep -q '"projects_dir"' "$CONFIG_DIR/config.json" 2>/dev/null; then
        return 0
    elif [ "$YES" = 0 ] && [ "$DRY" = 0 ] && tty_ok; then
        "$tool" setup </dev/tty >/dev/tty
        CHANGED=1
    else
        say later "projects folder: the first \`work\` asks (or rerun with --projects DIR)"
    fi
}

do_install() {
    say repo "$REPO"
    link "$REPO/bin/tmux-workspace" "$BIN_DIR/tmux-workspace"
    link "$REPO/bin/tw-clip" "$BIN_DIR/tw-clip"
    link "$REPO/bin/tw-sound" "$BIN_DIR/tw-sound"
    link "$REPO/agent-status.conf" "$STATUS_CONF"
    [ "$TMUXCONF" = 0 ] || link_tmux_conf
    if has sesh; then link "$REPO/sesh.toml" "$HOME_CONFIG/sesh/sesh.toml"; fi
    if [ "$SHELLS" = 1 ]; then
        shell_rc "${ZDOTDIR:-$HOME}/.zshrc" zsh
        shell_rc "$HOME/.bashrc" bash
        if has fish || [ -d "$HOME_CONFIG/fish" ]; then
            link "$REPO/shell/work.fish" "$HOME_CONFIG/fish/functions/work.fish"
        fi
    fi
    migrate_config
    if [ "$AGENTS" = 1 ]; then
        act agents "Claude Code, Codex, omp and Neovim where installed" "$REPO/bin/install-integrations"
    fi
    if [ "$DRY" = 1 ]; then
        say would "ask for your projects folder unless it is set"
        say "done" "dry run: nothing changed"
        return 0
    fi
    onboard
    if [ "$CHANGED" = 0 ]; then
        say "done" "tmux-workspace already installed"
        return 0
    fi
    if [ "$TMUXCONF" = 1 ] && has tmux && tmux info >/dev/null 2>&1; then
        tmux source-file "$HOME/.tmux.conf" && say reload "the running tmux server"
    fi
    "$BIN_DIR/tmux-workspace" doctor >/dev/null 2>&1 || true
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) say note "$BIN_DIR is not on PATH yet; new shells get it from the work command block" ;;
    esac
    say "done" "tmux-workspace installed"
    cat <<'EOF'

Next: open a new terminal, then
  work my-project      open or create a workspace
  Ctrl+a A             dashboard of every pane and agent
  Ctrl+a ?             every shortcut
  tmux-workspace doctor
EOF
}

do_uninstall() {
    local dest rc target dir manifest original saved kind
    for dest in "$BIN_DIR/tmux-workspace" "$BIN_DIR/tw-clip" "$BIN_DIR/tw-sound" "$STATUS_CONF" \
        "$HOME/.tmux.conf" "$HOME_CONFIG/sesh/sesh.toml" "$HOME_CONFIG/fish/functions/work.fish"; do
        if [ -L "$dest" ]; then
            case $(resolve "$dest") in
                "$REPO"/*) act remove "$dest" rm -f "$dest" ;;
            esac
        fi
    done
    for rc in "${ZDOTDIR:-$HOME}/.zshrc" "$HOME/.bashrc"; do
        [ -e "$rc" ] || continue
        target=$(resolve "$rc")
        if grep -qF "$BLOCK_START" "$target"; then act edit "$rc (removes the work command)" strip_block "$target"; fi
    done
    # Newest backups first; a path that is back already keeps its newer content.
    while IFS= read -r dir; do
        manifest=$dir/manifest
        [ -f "$manifest" ] || continue
        while IFS=$'\t' read -r original saved kind; do
            if [ ! -e "$original" ] && [ ! -L "$original" ] && { [ -e "$saved" ] || [ -L "$saved" ]; }; then
                act restore "$original ($kind)" mv "$saved" "$original"
            fi
        done <"$manifest"
        [ "$DRY" = 1 ] || mv "$manifest" "$manifest.restored"
    done < <(find "$STATE/backups" -mindepth 1 -maxdepth 1 -name 'install-*' 2>/dev/null | sort -r)
    if [ "$CHANGED" = 0 ]; then say "done" "nothing to remove"; else say "done" "tmux-workspace removed; snapshots and $CONFIG_DIR stay"; fi
}

main() {
    REPO=$(script_dir || true)
    if [ -z "$REPO" ] || [ ! -f "$REPO/bin/tmux-workspace" ]; then
        remote_install
    fi
    detect_platform
    if [ "$UNINSTALL" = 1 ]; then
        do_uninstall
        return 0
    fi
    check_requirements
    do_install
}

main
