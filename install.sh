#!/usr/bin/env bash
# tmux-workspace installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/ahmmkh/tmux-workspace/main/install.sh | bash
#   ./install.sh [options]          from a clone or an unpacked release; links point into it
#
# Piped, it downloads the release archive of this machine, checks it against the release's
# SHA256SUMS before writing anything, and unpacks it to ~/.local/share/tmux-workspace/<version>, which
# `current` links to. From a clone it builds the binary with cargo, or downloads the release of the
# clone's version. Every file it replaces moves to ~/.local/state/tmux-workspace/backups/ and
# --uninstall puts it back. Running it twice changes nothing the second time. Bash 3.2 compatible.
set -euo pipefail

TMUX_MIN=3.3
PYTHON_MIN=3.9
REPO_URL=https://github.com/ahmmkh/tmux-workspace
RELEASES=$REPO_URL/releases
BLOCK_START='# >>> tmux-workspace >>>'
BLOCK_END='# <<< tmux-workspace <<<'

usage() {
    cat <<'EOF'
Usage: install.sh [options]

  --deps            install missing tools (tmux, fzf, zoxide) with the package manager
  --projects DIR    the folder that holds your projects (otherwise you are asked once)
  --agents          also register Claude Code, Codex, omp and Neovim integrations
  --no-shell        do not add the `work` command to ~/.zshrc, ~/.bashrc or fish
  --no-tmux-conf    do not link ~/.tmux.conf
  --yes, -y         answer yes: replace an existing ~/.tmux.conf (a backup is kept)
  --version vX.Y.Z  install that release instead of the latest
  --dry-run         print what would change and change nothing
  --uninstall       remove the links and shell lines, and restore the backups
  -h, --help        show this help

Environment:
  TMUX_WORKSPACE_HOME    where releases are unpacked (~/.local/share/tmux-workspace)
  TMUX_WORKSPACE_DIST    a URL or folder holding the release archives and SHA256SUMS
  TMUX_WORKSPACE_BINARY  in a clone: install this binary instead of building or downloading one
EOF
}

say() { printf '%-8s %s\n' "$1" "$2"; }
die() { printf 'error    %s\n' "$1" >&2; exit "${2:-1}"; }

DEPS=0 AGENTS=0 SHELLS=1 TMUXCONF=1 DRY=0 YES=0 UNINSTALL=0 PROJECTS='' WANTED=''
ORIGINAL_ARGS=("$@")
while [ $# -gt 0 ]; do
    case $1 in
        --deps) DEPS=1 ;;
        --agents) AGENTS=1 ;;
        --projects) [ $# -ge 2 ] || { usage >&2; exit 2; }; PROJECTS=$2; shift ;;
        --projects=*) PROJECTS=${1#*=} ;;
        --version) [ $# -ge 2 ] || { usage >&2; exit 2; }; WANTED=$2; shift ;;
        --version=*) WANTED=${1#*=} ;;
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
if [ -n "$WANTED" ]; then
    if printf '%s\n' "$WANTED" | grep -Eq '^v?[0-9]+[.][0-9]+[.][0-9]+([-+][0-9A-Za-z.-]+)?$'; then
        WANTED=v${WANTED#v}
    else
        printf 'error    --version needs a release like v2.0.0, not %s\n' "$WANTED" >&2
        exit 2
    fi
fi
if [ -n "${TMUX_WORKSPACE_DIST:-}" ] && [ -d "$TMUX_WORKSPACE_DIST" ]; then
    TMUX_WORKSPACE_DIST=$(cd "$TMUX_WORKSPACE_DIST" && pwd -P)
fi

HOME_CONFIG=${XDG_CONFIG_HOME:-$HOME/.config}
BIN_DIR=$HOME/.local/bin
STATE=$HOME/.local/state/tmux-workspace
CONFIG_DIR=$HOME_CONFIG/tmux-workspace
STATUS_CONF=$HOME/.config/tmux-workspace/agent-status.conf  # tmux.conf sources this fixed path
CHANGED=0
BACKUP_DIR=''
CHECKED=0       # the requirements were checked
KIND=''         # what REPO is: a clone, or an unpacked release
HOME_SHARE=''   # for a release: the folder of the unpacked versions and the `current` link
OWN=''          # links that lead under this folder belong to the installation
STAGE=''        # a temporary folder with a download; removed on exit
STAGED=''       # the release unpacked in it

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

release_version() { head -n 1 "$1/VERSION" | tr -d '[:space:]'; }  # of an unpacked release

tree_version() {  # of a clone: the package version in Cargo.toml
    sed -n 's/^version = "\([^"]*\)".*/\1/p' "$1/Cargo.toml" | head -n 1
}

# Remove the temporary folder of a download. Every exit runs it.
remove_stage() {
    if [ -n "$STAGE" ] && [ -d "$STAGE" ]; then rm -rf "${STAGE:?}"; fi
    STAGE=''
}
trap remove_stage EXIT

# ---------------------------------------------------------------------------------------------
# Where the code lives: this clone or unpacked release, or a release downloaded when the script
# arrives through a pipe.

script_dir() {
    local source=${BASH_SOURCE[0]:-}
    [ -n "$source" ] && [ -f "$source" ] || return 1
    (cd "$(dirname "$source")" && pwd -P)
}

PM='' SUDO=''
detect_platform() {
    # Package managers live in sbin, which a non-root login PATH may lack (busybox su on Alpine).
    PATH=$PATH:/usr/local/sbin:/usr/sbin:/sbin
    local pm
    for pm in brew apt-get dnf pacman apk zypper; do
        if has "$pm"; then PM=$pm; break; fi
    done
    if [ "$PM" != brew ] && [ "$(id -u)" != 0 ]; then SUDO=sudo; fi
}

install_command() {  # install_command TOOL...  prints the command a person would run
    local names='' tool
    for tool in "$@"; do names="$names $tool"; done
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
    [ $# -gt 0 ] || return 0
    [ -n "$PM" ] || die "no supported package manager found; install$(printf ' %s' "$@") yourself" 3
    if [ -n "$SUDO" ] && ! has sudo; then die "sudo is needed to run: $(install_command "$@")" 3; fi
    if [ "$PM" = apt-get ]; then $SUDO apt-get update -qq; fi
    say install "$*"
    case $PM in
        brew) brew install "$@" ;;
        apt-get) DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq "$@" ;;
        dnf) $SUDO dnf install -y -q "$@" ;;
        pacman) $SUDO pacman -S --needed --noconfirm "$@" ;;
        apk) $SUDO apk add -q "$@" ;;
        zypper) $SUDO zypper install -y "$@" ;;
    esac
}

# Whether DIR holds tmux-workspace: a clone (with Cargo.toml) or an unpacked release (with VERSION).
is_tree() {
    [ -f "$1/tmux.conf" ] && [ -f "$1/agent-status.conf" ] && { [ -f "$1/Cargo.toml" ] || [ -f "$1/VERSION" ]; }
}

# Where releases are unpacked: a folder per version and a `current` link to one of them.
share_home() {
    local dir=${TMUX_WORKSPACE_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/tmux-workspace}
    case $dir in /*) ;; *) dir=$PWD/$dir ;; esac
    printf '%s\n' "$dir"
}

# The Rust target of this machine's release archive, or nothing when no release is built for it.
detect_target() {
    case "$(uname -s)/$(uname -m)" in
        Darwin/arm64 | Darwin/aarch64) echo aarch64-apple-darwin ;;
        Darwin/x86_64) echo x86_64-apple-darwin ;;
        Linux/x86_64 | Linux/amd64) echo x86_64-unknown-linux-musl ;;
        Linux/aarch64 | Linux/arm64) echo aarch64-unknown-linux-musl ;;
        *) echo '' ;;
    esac
}

dist_base() {  # dist_base TAG: where the files of release TAG (vX.Y.Z or latest) are
    if [ -n "${TMUX_WORKSPACE_DIST:-}" ]; then
        printf '%s\n' "${TMUX_WORKSPACE_DIST%/}"
    elif [ "$1" = latest ]; then
        printf '%s\n' "$RELEASES/latest/download"
    else
        printf '%s\n' "$RELEASES/download/$1"
    fi
}

fetch() {  # fetch URL DEST: download with curl or wget. A URL without a scheme is a local file.
    local url=$1 dest=$2
    case $url in
        *://*) ;;
        *) cp "$url" "$dest" 2>/dev/null || die "could not read $url" 3; return 0 ;;
    esac
    if ! has curl && ! has wget && [ "$DEPS" = 1 ] && [ "$DRY" = 0 ]; then install_packages curl; fi
    if has curl; then
        curl -fsSL --retry 2 -o "$dest" "$url" || die "could not download $url" 3
    elif has wget; then
        wget -q -O "$dest" "$url" || die "could not download $url" 3
    else
        die "need curl or wget to download tmux-workspace" 3
    fi
}

verify() {  # verify FILE SUMS: FILE has the SHA-256 that SUMS (sha256sum format) lists for its name
    local file=$1 name=${1##*/} want got
    want=$(awk -v name="$name" '$2 == name || $2 == "*" name { print $1; exit }' "$2")
    if has sha256sum; then
        got=$(sha256sum "$file" | awk '{ print $1 }')
    elif has shasum; then
        got=$(shasum -a 256 "$file" | awk '{ print $1 }')
    else
        die "need sha256sum or shasum to check the download" 3
    fi
    if [ -z "$want" ] || [ "$want" != "$got" ]; then die "checksum mismatch for $name; nothing was installed" 3; fi
}

# stage_archive TAG: download this machine's archive of release TAG (vX.Y.Z or latest) and the
# release's SHA256SUMS to a temporary folder, check the one against the other, and unpack it. Sets
# STAGE, the folder (removed on exit), and STAGED, the release in it. Writes nothing else.
stage_archive() {
    local target name dir base
    target=$(detect_target)
    if [ -z "$target" ]; then
        if has cargo; then
            die "no prebuilt binary for $(uname -s)/$(uname -m); clone $REPO_URL and run its install.sh, which builds one with cargo" 3
        fi
        die "no prebuilt binary for $(uname -s)/$(uname -m); install Rust (https://rustup.rs) and rerun" 3
    fi
    has tar || die "need tar to unpack tmux-workspace" 3
    base=$(dist_base "$1")
    name=tmux-workspace-$target.tar.gz
    remove_stage
    STAGE=$(mktemp -d)
    say download "$base/$name"
    fetch "$base/$name" "$STAGE/$name"
    fetch "$base/SHA256SUMS" "$STAGE/SHA256SUMS"
    verify "$STAGE/$name" "$STAGE/SHA256SUMS"
    mkdir "$STAGE/release"
    tar -xzf "$STAGE/$name" -C "$STAGE/release" || die "could not unpack $name" 3
    rm -f "${STAGE:?}/${name:?}"
    STAGED=''
    for dir in "$STAGE"/release/tmux-workspace-*; do
        if [ -f "$dir/VERSION" ] && [ -f "$dir/install.sh" ] && [ -x "$dir/bin/tmux-workspace" ]; then STAGED=$dir; fi
    done
    [ -n "$STAGED" ] || die "$name holds no tmux-workspace release" 3
}

# run_staged: hand over to the installer of the release just downloaded, with the same options. It
# copies its release next to the other versions, links from there, and removes the download.
run_staged() {
    TMUX_WORKSPACE_HOME=${HOME_SHARE:-$(share_home)}
    TMUX_WORKSPACE_STAGE=$STAGE
    export TMUX_WORKSPACE_HOME TMUX_WORKSPACE_STAGE
    exec bash "$STAGED/install.sh" ${ORIGINAL_ARGS[@]+"${ORIGINAL_ARGS[@]}"}
}

# Piped (`curl … | bash`): no clone or release is around this script. Download the release, and its
# own installer does the rest.
remote_install() {
    local home
    home=$(share_home)
    detect_platform
    if [ "$UNINSTALL" = 1 ]; then
        if [ -f "$home/current/install.sh" ]; then
            exec bash "$home/current/install.sh" ${ORIGINAL_ARGS[@]+"${ORIGINAL_ARGS[@]}"}
        fi
        say "done" "nothing to remove"
        exit 0
    fi
    stage_archive "${WANTED:-latest}"
    HOME_SHARE=$home
    run_staged
}

# Whether this installer runs from the download that run_staged handed over: REPO is in
# TMUX_WORKSPACE_STAGE, which is removed once it is done. A stray value of it is ignored.
staged_here() {
    local stage=${TMUX_WORKSPACE_STAGE:-}
    [ -n "$stage" ] && [ -d "$stage/release" ] || return 1
    stage=$(cd "$stage" && pwd -P)
    case $REPO in "$stage"/release/*) return 0 ;; esac
    return 1
}

# REPO is an unpacked release. An installed one is <home>/<version> and runs as <home>/current, so
# that links and the shell block outlive updates. One unpacked anywhere else (the download of a piped
# install, a folder you unpacked yourself) is copied there first, once the requirements are met.
settle_release() {
    local parent
    parent=$(dirname "$REPO")
    if staged_here; then
        STAGE=$(cd "$TMUX_WORKSPACE_STAGE" && pwd -P)  # the download this installer runs from: removed on exit
        unset TMUX_WORKSPACE_STAGE
    elif [ -L "$parent/current" ]; then
        HOME_SHARE=$parent
        if [ -n "$WANTED" ] && [ "${WANTED#v}" != "$(release_version "$REPO")" ] && [ "$UNINSTALL" = 0 ]; then
            stage_archive "$WANTED"
            run_staged
        fi
        if [ "$UNINSTALL" = 1 ] || [ "$(resolve "$parent/current")" = "$REPO" ]; then
            REPO=$parent/current
            return 0
        fi
        check_requirements
        place_release "$parent"  # an earlier version, run as it is: it becomes current again
        return 0
    fi
    if [ "$UNINSTALL" = 1 ]; then
        HOME_SHARE=$(share_home)
        if [ -L "$HOME_SHARE/current" ]; then REPO=$HOME_SHARE/current; fi
        return 0
    fi
    check_requirements
    place_release "$(share_home)"
}

# place_release HOME: make REPO the release that HOME/current runs. It is copied to HOME/<version>
# unless that version is there already, `current` links to it, and of the other versions only the
# one that was current until now stays.
place_release() {
    local home=$1 own dest previous
    own=$(release_version "$REPO")
    dest=$home/$own
    HOME_SHARE=$home
    if [ "$DRY" = 1 ]; then
        say would "unpack tmux-workspace $own to $dest"
        say would "link $home/current -> $own"
        REPO=$home/current
        return 0
    fi
    mkdir -p "$home"
    if [ ! -f "$dest/VERSION" ] || [ ! -x "$dest/bin/tmux-workspace" ]; then
        rm -rf "${home:?}/.incoming-$$" "${home:?}/.outgoing-$$"
        cp -pR "$REPO" "$home/.incoming-$$"
        if [ -e "$dest" ] || [ -L "$dest" ]; then mv "$dest" "$home/.outgoing-$$"; fi
        mv "$home/.incoming-$$" "$dest"
        rm -rf "${home:?}/.outgoing-$$"
        CHANGED=1
        say unpack "tmux-workspace $own to $dest"
    fi
    previous=$(readlink "$home/current" 2>/dev/null || true)
    if [ "$previous" != "$own" ]; then
        ln -sfn "$own" "$home/current"
        CHANGED=1
        say link "$home/current -> $own"
    fi
    prune_releases "$home" "$own" "$previous"
    REPO=$home/current
}

prune_releases() {  # prune_releases HOME KEEP...: remove the other unpacked versions
    local home=$1 dir name
    shift
    for dir in "$home"/*; do
        name=${dir##*/}
        if [ -L "$dir" ] || [ ! -f "$dir/VERSION" ]; then continue; fi
        case " $* " in *" $name "*) continue ;; esac
        rm -rf "${dir:?}"
        say remove "$dir"
    done
}

# ---------------------------------------------------------------------------------------------
# The binary of a clone: given, built with cargo, or downloaded

# place_binary SOURCE DEST STAMP VERSION: copy SOURCE to DEST through a new file, so a running DEST
# is never written into (macOS kills a signed binary changed in place). STAMP records the release
# VERSION the binary came from; a built or given binary has none.
place_binary() {
    mkdir -p "$(dirname "$2")"
    cp "$1" "$2.new.$$"
    chmod 755 "$2.new.$$"
    mv -f "$2.new.$$" "$2"
    if [ -n "$4" ]; then printf '%s\n' "$4" >"$3"; else rm -f "${3:?}"; fi
}

install_binary() {
    local dest=$REPO/bin/tmux-workspace stamp=$REPO/bin/tmux-workspace.version
    local given=${TMUX_WORKSPACE_BINARY:-} version built
    if [ -n "$given" ]; then
        [ -f "$given" ] || die "TMUX_WORKSPACE_BINARY is not a file: $given" 3
        if [ "$(resolve "$given")" = "$(resolve "$dest")" ] || cmp -s "$given" "$dest" 2>/dev/null; then return 0; fi
        act binary "$dest (from $given)" place_binary "$given" "$dest" "$stamp" ''
    elif has cargo; then
        if [ "$DRY" = 1 ]; then say would "build $dest with cargo"; return 0; fi
        say build "cargo build --release --locked"
        (cd "$REPO" && cargo build --release --locked) || die "cargo build failed" 3
        built=${CARGO_TARGET_DIR:-$REPO/target}/release/tmux-workspace
        [ -f "$built" ] || die "cargo build made no $built" 3
        if cmp -s "$built" "$dest" 2>/dev/null; then
            rm -f "${stamp:?}"
            return 0
        fi
        act binary "$dest (built)" place_binary "$built" "$dest" "$stamp" ''
    else
        version=$(tree_version "$REPO")
        [ -n "$version" ] || die "no version in $REPO/Cargo.toml" 3
        if [ -x "$dest" ] && [ "$(cat "$stamp" 2>/dev/null || true)" = "$version" ]; then return 0; fi
        if [ "$DRY" = 1 ]; then say would "download tmux-workspace $version to $dest"; return 0; fi
        stage_archive "v$version"
        act binary "$dest (release $version)" place_binary "$STAGED/bin/tmux-workspace" "$dest" "$stamp" "$version"
        remove_stage
    fi
}

# ---------------------------------------------------------------------------------------------
# Requirements

check_requirements() {
    [ "$CHECKED" = 0 ] || return 0
    CHECKED=1
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
    # Only the commands not ported from Python yet need it (stages 2 and 3 of the Rust port).
    version=$(python_version)
    if [ -n "$version" ] && version_ge "$version" "$PYTHON_MIN"; then
        say ok "python3 $version"
    else
        say warn "python3 $PYTHON_MIN+ not found: dash, ai and cleanup need it until they are ported"
    fi
    local optional='' recommended='' upgrade=''
    for tool in fzf zoxide git sesh lazygit; do
        if has "$tool"; then say ok "$tool"; else optional="$optional $tool"; fi
    done
    for tool in fzf zoxide; do
        has "$tool" || recommended="$recommended $tool"
    done
    [ -z "$old" ] || upgrade=" tmux"

    if [ "$DEPS" = 1 ] && [ "$DRY" = 0 ] && [ -n "$missing$upgrade$recommended" ]; then
        # shellcheck disable=SC2086
        install_packages $missing $upgrade $recommended
        check_requirements_again
        return
    fi
    if [ -n "$missing" ] || [ -n "$old" ]; then
        [ -z "$old" ] || say old "$old"
        # shellcheck disable=SC2086
        [ -z "$missing" ] || say missing "$(install_command $missing)"
        die "tmux-workspace needs tmux $TMUX_MIN+; rerun with --deps to install it" 3
    fi
    for tool in $optional; do
        case $tool in
            fzf) say optional "fzf: fuzzy workspace and tab pickers ($(install_command fzf))" ;;
            zoxide) say optional "zoxide: \`work\` finds folders you visit often" ;;
            git) say optional "git: lazygit and updating a clone use it" ;;
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

# Whether PATH leads into this installation: the clone, or any unpacked version of the release.
owned() {
    case $(resolve "$1") in "$OWN"/*) return 0 ;; esac
    return 1
}

link() {  # link SOURCE DEST
    local source=$1 dest=$2
    ours "$dest" "$source" && return 0
    if [ -e "$dest" ] || [ -L "$dest" ]; then backup "$dest"; fi
    act link "$dest -> $source" make_link "$source" "$dest"
}

# A file the user already has (not one of ours) is replaced only with --yes or a yes on the terminal.
link_asking() {  # link_asking SOURCE DEST
    local source=$1 dest=$2
    ours "$dest" "$source" && return 0
    if { [ -e "$dest" ] || [ -L "$dest" ]; } && [ "$YES" = 0 ]; then
        if [ "$DRY" = 1 ] || ! ask "Replace your $dest? It moves to a backup and --uninstall restores it."; then
            say skip "$dest exists; rerun with --yes to replace it (a backup is kept)"
            return 0
        fi
    fi
    link "$source" "$dest"
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
        "$tool" setup </dev/tty >/dev/tty || say later "projects folder: the first \`work\` asks"
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
    [ "$TMUXCONF" = 0 ] || link_asking "$REPO/tmux.conf" "$HOME/.tmux.conf"
    if has sesh; then link_asking "$REPO/sesh.toml" "$HOME_CONFIG/sesh/sesh.toml"; fi
    if [ "$SHELLS" = 1 ]; then
        shell_rc "${ZDOTDIR:-$HOME}/.zshrc" zsh
        shell_rc "$HOME/.bashrc" bash
        if has fish || [ -d "$HOME_CONFIG/fish" ]; then
            link_asking "$REPO/shell/work.fish" "$HOME_CONFIG/fish/functions/work.fish"
        fi
    fi
    migrate_config
    if [ "$AGENTS" = 1 ]; then
        act agents "Claude Code, Codex, omp and Neovim where installed" \
            env TMUX_WORKSPACE_SHARE="$REPO" "$REPO/bin/tmux-workspace" integrations install
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
    say next "open a new terminal, then: work my-project   (Ctrl+a ? lists every shortcut)"
}

stop_server_hooks() {
    tmux set-option -gu default-command
    tmux set-hook -gu session-created
}

do_uninstall() {
    local dest rc target dir manifest original saved kind ours_conf=0 line
    if [ -L "$HOME/.tmux.conf" ] && owned "$HOME/.tmux.conf"; then ours_conf=1; fi
    if [ "$DRY" = 1 ]; then
        say would "remove the agent integrations that --agents added"
    elif [ -x "$REPO/bin/tmux-workspace" ]; then
        TMUX_WORKSPACE_SHARE="$REPO" "$REPO/bin/tmux-workspace" integrations uninstall | while IFS= read -r line; do
            if [ -n "$line" ]; then say remove "$line"; fi
        done
    fi
    for dest in "$BIN_DIR/tmux-workspace" "$BIN_DIR/tw-clip" "$BIN_DIR/tw-sound" "$STATUS_CONF" \
        "$HOME/.tmux.conf" "$HOME_CONFIG/sesh/sesh.toml" "$HOME_CONFIG/fish/functions/work.fish"; do
        if [ -L "$dest" ] && owned "$dest"; then act remove "$dest" rm -f "${dest:?}"; fi
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
    # A running server still starts panes with the removed tmux-workspace: tell it to stop.
    if [ "$ours_conf" = 1 ] && has tmux && tmux info >/dev/null 2>&1; then
        act tmux "the running server no longer starts panes through tmux-workspace" stop_server_hooks
        say note "restart tmux to finish: tmux kill-server, then start it again"
    fi
    if [ "$CHANGED" = 0 ]; then say "done" "nothing to remove"; else say "done" "tmux-workspace removed; snapshots and $CONFIG_DIR stay"; fi
    if [ "$KIND" = release ] && [ "$CHANGED" = 1 ]; then
        say note "the program files stay in $HOME_SHARE; delete that folder to remove them"
    fi
}

main() {
    REPO=$(script_dir || true)
    if [ -z "$REPO" ] || ! is_tree "$REPO"; then
        remote_install
    fi
    detect_platform
    if [ -f "$REPO/Cargo.toml" ]; then
        KIND=clone
        OWN=$REPO
        if [ -n "$WANTED" ]; then say note "--version is for release installs; in a clone, check out $WANTED instead"; fi
    else
        KIND=release
        settle_release
        OWN=$(cd "$HOME_SHARE" 2>/dev/null && pwd -P || printf '%s' "$HOME_SHARE")
    fi
    if [ "$UNINSTALL" = 1 ]; then
        do_uninstall
        return 0
    fi
    check_requirements
    if [ "$KIND" = clone ]; then install_binary; fi
    do_install
}

main
