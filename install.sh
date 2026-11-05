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
# rust-version in Cargo.toml: what building from source takes.
RUST_MIN=1.89
VERSION_RE='[0-9]+[.][0-9]+[.][0-9]+([-+][0-9A-Za-z.-]+)?'
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
  --update          update an install to the latest release (a clone: git pull), keeping
                    its choices; `tmux-workspace update` runs this
  --check           with --update: only say whether there is an update
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
die() { DIE_MSG=$1; printf 'error    %s\n' "$1" >&2; exit "${2:-1}"; }
DIE_MSG=''  # what die said: on a terminal, the failure says it again as what to do

DEPS=0 AGENTS=0 SHELLS=1 TMUXCONF=1 DRY=0 YES=0 UNINSTALL=0 UPDATE=0 CHECK=0 PROJECTS='' WANTED=''
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
        --update) UPDATE=1 ;;
        --check) CHECK=1 ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'unknown option: %s\n\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done
if { [ "$CHECK" = 1 ] && [ "$UPDATE" = 0 ]; } || { [ "$UPDATE" = 1 ] && [ "$UNINSTALL" = 1 ]; }; then
    printf '%s\n\n' '--check goes with --update, and --update not with --uninstall' >&2
    usage >&2
    exit 2
fi
# An update hands over to the installer of the new version; this names the version it came from.
UPDATE_FROM=${TMUX_WORKSPACE_UPDATE_FROM:-}
unset TMUX_WORKSPACE_UPDATE_FROM
if [ -n "$WANTED" ]; then
    if printf '%s\n' "$WANTED" | grep -Eq "^v?$VERSION_RE\$"; then
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
PENDING=''      # a new binary next to the one it replaces after a pull; removed on exit unless moved
REASON='' REASON_KIND=''  # why try_fetch or try_stage_archive failed; missing: no release fits
UPDATED_FROM='' # what git_update updated from: nothing when the clone was up to date
NEW_BINARY='' NEW_STAMP=''  # the binary git_update had for the new version, and its release
CHANGES=0       # how many changes were made so far: a progress line says when its step made none
TMUXCONF_AGREED=$YES  # 1: replace a ~/.tmux.conf of the user's (moved to a backup) without asking
PIPED=0         # this run started as `curl … | bash`

# How this run talks to the person. plain: the `say` lines above, which tests and scripts parse. guided:
# a checklist of what to set up, then a line per step and a summary. styled: the lines per step and the
# summary without the checklist (--update, --uninstall). See choose_ui.
UI=plain
LOG=$STATE/install.log  # guided and styled runs: what every command printed, with a header per run
LOG_START=0     # the lines LOG had before this run
UI_OPEN=0       # 1 once the lines per step started: the terminal is on fd 3, stdout and stderr go to LOG
PENDING_LOG=''  # a temporary LOG while nothing is decided yet (a piped download): kept only on failure
STEP='' STEP_NAME='' STEP_MODE='' STEP_FIX='' STEP_NOTE='' STEP_CHANGES=0 SPIN_PID=''
SAME_NOTE='already set up'  # what a line says when its step changed nothing
TTY_SAVED=''    # the terminal settings the checklist changed, restored on every exit
CURSOR_HIDDEN=0
DONE_NAMES=''   # the steps that changed something, for the summary
KEPT=''         # files of the user's that were left in place, a line each
CODEX_CHANGED=0 # the agent step changed Codex's hooks, which Codex asks to review
COLS=80
G_LOCK='#' G_CURSOR='>' G_OK='ok' G_FAIL='x' G_DOT='-' G_UPDOWN='up/down' G_MORE='...'
SPIN_FRAMES=('|' '/' '-' "\\")
C_BOLD='' C_DIM='' C_GREEN='' C_RED='' C_CYAN='' C_OFF=''

# ---------------------------------------------------------------------------------------------
# Helpers

# act LABEL TEXT COMMAND... runs COMMAND, or only prints it on a dry run.
act() {
    local label=$1 text=$2
    shift 2
    changed
    if [ "$DRY" = 1 ]; then
        say would "$label $text"
    else
        "$@"
        say "$label" "$text"
    fi
}

changed() { CHANGED=1; CHANGES=$((CHANGES + 1)); }

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

version_lt() {  # version_lt 2.0.0 2.0.1: the first X.Y.Z is older
    awk -v a="${1%%[-+]*}" -v b="${2%%[-+]*}" 'BEGIN {
        split(a, x, "."); split(b, y, ".")
        for (i = 1; i <= 3; i++) { if (x[i] + 0 < y[i] + 0) exit 0; if (x[i] + 0 > y[i] + 0) exit 1 }
        exit 1
    }'
}

ref_version() {  # ref_version ROOT REF: the package version in Cargo.toml at REF of the clone at ROOT
    { git -C "$1" show "$2:Cargo.toml" 2>/dev/null || true; } | awk -F'"' '/^version = "/ { print $2; exit }'
}

# clone_label ROOT REF: the version of the clone at REF and its commit, as in "2.0.1 1a2b3c4". Before
# the Rust port there was no Cargo.toml: the newest CHANGELOG heading names the version then.
clone_label() {
    local version
    version=$(ref_version "$1" "$2")
    if [ -z "$version" ]; then
        version=$({ git -C "$1" show "$2:CHANGELOG.md" 2>/dev/null || true; } | awk '/^## [0-9]/ { print $2; exit }')
    fi
    printf '%s %s\n' "${version:-unknown}" "$(git -C "$1" rev-parse --short "$2")"
}

# Remove the temporary folder of a download.
remove_stage() {
    if [ -n "$STAGE" ] && [ -d "$STAGE" ]; then rm -rf "${STAGE:?}"; fi
    STAGE=''
}

on_exit() {  # every exit: no download and no half-placed binary stays behind, and the terminal is as it was
    local status=$?
    remove_stage
    if [ -n "$PENDING" ]; then rm -f "${PENDING:?}"; fi
    if [ "$UI" != plain ]; then ui_exit "$status" || true; fi
}
trap on_exit EXIT

# ---------------------------------------------------------------------------------------------
# On a terminal: a line per step instead of the `say` lines, which go to LOG. Plain ANSI escapes,
# stty and read; nothing to install first.

# A person at a terminal that moves its cursor: not a pipe, a file, CI or a dumb terminal.
terminal_ok() {
    [ -t 1 ] && [ -z "${CI:-}" ] && has stty && tty_ok || return 1
    case ${TERM:-dumb} in dumb | unknown) return 1 ;; esac
}

# The checklist for an install on a terminal; the lines per step for an update or an uninstall there (and
# for the installer a pull or a download handed over to). --yes, --dry-run and --check stay plain.
choose_ui() {
    [ "$DRY$YES$CHECK" = 000 ] && terminal_ok || return 0
    if [ "$UPDATE$UNINSTALL" = 00 ] && [ -z "$UPDATE_FROM" ]; then UI=guided; else UI=styled; fi
    if [ "$UPDATE" = 1 ]; then SAME_NOTE=unchanged; fi
    case ${LC_ALL:-${LC_CTYPE:-${LANG:-}}} in
        *[Uu][Tt][Ff]-8* | *[Uu][Tt][Ff]8*)
            G_LOCK='■' G_CURSOR='›' G_OK='✓' G_FAIL='✗' G_DOT='·' G_UPDOWN='↑↓' G_MORE='…'
            SPIN_FRAMES=('⠋' '⠙' '⠹' '⠸' '⠼' '⠴' '⠦' '⠧' '⠇' '⠏')
            ;;
    esac
    if [ -z "${NO_COLOR:-}" ]; then
        C_BOLD=$'\033[1m' C_DIM=$'\033[2m' C_GREEN=$'\033[32m' C_RED=$'\033[31m' C_CYAN=$'\033[36m' C_OFF=$'\033[0m'
    fi
    COLS=$(stty size </dev/tty 2>/dev/null | awk '{ print $2 }')
    case $COLS in '' | 0 | *[!0-9]*) COLS=80 ;; esac
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
}

ui() {  # ui TEXT: a line on the terminal
    if [ "$UI_OPEN" = 1 ]; then printf '%s\n' "$1" >&3; else printf '%s\n' "$1"; fi
}

ui_path() {  # PATH with the home folder shown as ~
    case $1 in
        "$HOME") printf '~\n' ;;
        "$HOME"/*)
            # shellcheck disable=SC2088 # a ~ to read, not to expand
            printf '~/%s\n' "${1#"$HOME"/}"
            ;;
        *) printf '%s\n' "$1" ;;
    esac
}

fit() {  # fit TEXT [WIDTH]: TEXT cut to WIDTH characters (the terminal's), so that it never wraps
    local width=${2:-$((COLS - 1))}
    if [ "${#1}" -le "$width" ]; then
        printf '%s' "$1"
    elif [ "$width" -gt "${#G_MORE}" ]; then
        printf '%s%s' "${1:0:$((width - ${#G_MORE}))}" "$G_MORE"
    fi
}

ui_version() {  # the version this installer installs
    if [ -n "${REPO:-}" ] && [ -f "$REPO/VERSION" ]; then
        release_version "$REPO"
    elif [ -n "${REPO:-}" ] && [ -f "$REPO/Cargo.toml" ]; then
        tree_version "$REPO"
    fi
}

ui_rerun() {  # the command that runs this installer again
    if [ "$PIPED" = 1 ] || [ -z "${REPO:-}" ]; then
        printf '%s\n' 'curl -fsSL https://raw.githubusercontent.com/ahmmkh/tmux-workspace/main/install.sh | bash'
    else
        printf '%s/install.sh\n' "$(ui_path "$REPO")"
    fi
}

# From here on the terminal shows a line per step (on fd 3), and stdout and stderr go to LOG.
progress_open() {
    [ "$UI" != plain ] && [ "$UI_OPEN" = 0 ] || return 0
    local version
    if [ -n "$PENDING_LOG" ]; then
        LOG=$PENDING_LOG
    else
        mkdir -p "$STATE"
        chmod 700 "$STATE"
        (umask 077 && : >>"$LOG")
        chmod 600 "$LOG"
    fi
    LOG_START=$(awk 'END { print NR }' "$LOG")
    version=$(ui_version)
    exec 3>&1 4>&2 >>"$LOG" 2>&1
    UI_OPEN=1
    printf '=== %s  install.sh%s%s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "${version:+ $version}" \
        "${ORIGINAL_ARGS[*]+ ${ORIGINAL_ARGS[*]}}"
}

progress_close() {  # the terminal back on stdout and stderr: before handing over to another installer
    [ "$UI_OPEN" = 1 ] || return 0
    step_end
    exec 1>&3 2>&4 3>&- 4>&-
    UI_OPEN=0
    if [ -n "$PENDING_LOG" ]; then rm -f "$PENDING_LOG"; fi
    PENDING_LOG=''
}

# step NAME [DETAIL] [MODE]: the line of what runs next, marked done (ok) by the next step or step_end,
# or failed (x) when the installer stops first. NAME is also what a bug report names. MODE slow spins
# meanwhile; quick is a step that changes nothing the installer counts; otherwise, a step that changed
# nothing says so. Nothing in the plain UI.
step() {
    [ "$UI" != plain ] || return 0
    progress_open
    step_end
    STEP_NAME=$1 STEP_MODE=${3:-} STEP_FIX='' STEP_NOTE='' STEP_CHANGES=$CHANGES
    STEP=$(printf '%-22s%s' "$1" "${2:-}")
    STEP=$(fit "$STEP" $((COLS - 6)))
    printf -- '--- %s\n' "$STEP"
    if [ "$STEP_MODE" = slow ] && has sleep; then
        spin_start
    else
        printf '  %s %s' "${SPIN_FRAMES[0]}" "$STEP" >&3
    fi
}

step_end() {  # the step that runs is done
    [ -n "$STEP" ] || return 0
    spin_stop
    local note=$STEP_NOTE
    if [ "$STEP_MODE" = slow ] || [ "$CHANGES" != "$STEP_CHANGES" ]; then
        DONE_NAMES="${DONE_NAMES:+$DONE_NAMES, }$STEP_NAME"
    elif [ "$STEP_MODE" != quick ] && [ -z "$note" ]; then
        note=$SAME_NOTE
    fi
    mark_step "$C_GREEN$G_OK$C_OFF" "$STEP" "$note"
    STEP=''
}

mark_step() {  # mark_step GLYPH TEXT [NOTE]: the line of the step, finished
    printf '\r\033[K  %s %s%s\n' "$1" "$2" "${3:+ $C_DIM($3)$C_OFF}" >&3
}

spin_start() {  # the line of the step, with a spinner until spin_stop
    local text=$STEP
    (
        trap 'exit 0' TERM
        i=0
        while kill -0 "$$" 2>/dev/null; do
            printf '\r  %s %s' "${SPIN_FRAMES[i]}" "$text" >&3
            i=$(((i + 1) % ${#SPIN_FRAMES[@]}))
            sleep 0.1 2>/dev/null || sleep 1
        done
    ) &
    SPIN_PID=$!
}

spin_stop() {
    [ -n "$SPIN_PID" ] || return 0
    kill "$SPIN_PID" 2>/dev/null || true
    wait "$SPIN_PID" 2>/dev/null || true
    SPIN_PID=''
}

# ui_exit STATUS: every exit of a guided or styled run. The terminal gets back its settings and its
# cursor, and a run that stops in a step says why, what to do, and how to report it.
ui_exit() {
    spin_stop
    if [ -n "$TTY_SAVED" ]; then stty "$TTY_SAVED" </dev/tty 2>/dev/null; fi
    TTY_SAVED=''
    if [ "$CURSOR_HIDDEN" = 1 ]; then printf '\033[?25h' >/dev/tty; fi
    CURSOR_HIDDEN=0
    if [ "$UI_OPEN" = 1 ]; then
        if [ "$1" = 0 ]; then step_end; else progress_fail "$1"; fi
    else
        case $1 in 129 | 130 | 143) printf '\n%s\n' 'Cancelled. Nothing was changed.' >/dev/tty ;; esac
    fi
    if [ -n "$PENDING_LOG" ]; then rm -f "$PENDING_LOG"; fi
}

progress_fail() {  # progress_fail STATUS: the run stopped in the middle of a step
    local status=$1 name=${STEP_NAME:-install} fix line
    spin_stop
    mark_step "$C_RED$G_FAIL$C_OFF" "${STEP:-${DIE_MSG:-the installer stopped (exit status $status)}}"
    STEP=''
    case $status in
        129 | 130 | 143)
            ui "  Stopped. What was done so far stays; run the installer again to finish: $(ui_rerun)"
            return 0
            ;;
    esac
    keep_log
    ui ''
    log_tail 10 | while IFS= read -r line; do ui "    $C_DIM$line$C_OFF"; done
    fix=${DIE_MSG:-${STEP_FIX:-Fix what the log says, then run the installer again: $(ui_rerun)}}
    ui ''
    ui "  $C_BOLD$fix$C_OFF"
    ui "  Log: $(ui_path "$LOG")"
    ui "  Report it: $(report_link "$name")"
    copy_report
}

keep_log() {  # a failed piped download: its temporary log joins install.log
    [ -n "$PENDING_LOG" ] || return 0
    LOG=$STATE/install.log
    mkdir -p "$STATE"
    chmod 700 "$STATE"
    (umask 077 && : >>"$LOG")
    LOG_START=$(awk 'END { print NR }' "$LOG")
    cat "$PENDING_LOG" >>"$LOG"
    rm -f "$PENDING_LOG"
    PENDING_LOG=''
}

log_tail() {  # log_tail N: the last N lines this run wrote to LOG
    awk -v start="$LOG_START" -v n="$1" 'NR > start { line[NR] = $0 }
        END { first = NR - n + 1; if (first <= start) first = start + 1; for (i = first; i <= NR; i++) print line[i] }' "$LOG"
}

# ---------------------------------------------------------------------------------------------
# A failed step offers a bug report: a link to a new GitHub issue with this machine and the end of
# the log filled in, anonymized. Nothing is sent; the person opens the link, reads it and decides.

# anonymize: standard input with what names this person or machine taken out, the same way as the
# reports of `tmux-workspace bug`: the home folder, the user and host names, e-mail and IP addresses,
# credentials in URLs, and tokens.
anonymize() {
    local name home=$HOME real host short hosts=''
    local rules=(-e 's/([A-Za-z][A-Za-z0-9+.-]*:\/\/)[^\/@[:space:]:]+:[^\/@[:space:]]*@/\1/g'
        -e 's/[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/<email>/g')
    real=$(cd "$HOME" 2>/dev/null && pwd -P || true)
    for home in "$HOME" "$real"; do
        if [ "${#home}" -gt 1 ]; then rules+=(-e "s/$(ere_escape "$home")/~/g"); fi
    done
    if has hostname; then hosts=$(hostname 2>/dev/null || true); fi
    for host in "$hosts" "$(uname -n 2>/dev/null || true)"; do
        short=${host%%.*}
        for name in "$host" "$short"; do
            [ "${#name}" -ge 2 ] || continue
            name=$(ere_escape "$name")
            rules+=(-e "s/(^|[^[:alnum:]_-])$name([^[:alnum:]_-]|\$)/\1<host>\2/g")
            rules+=(-e "s/(^|[^[:alnum:]_-])$name([^[:alnum:]_-]|\$)/\1<host>\2/g")
        done
    done
    for name in "${USER:-}" "${LOGNAME:-}" "$(id -un 2>/dev/null || true)"; do
        [ "${#name}" -ge 2 ] || continue
        name=$(ere_escape "$name")
        rules+=(-e "s/(^|[^[:alnum:]_<])$name([^[:alnum:]_>]|\$)/\1<user>\2/g")
        rules+=(-e "s/(^|[^[:alnum:]_<])$name([^[:alnum:]_>]|\$)/\1<user>\2/g")
    done
    local ipv4='(^|[^[:alnum:].])([0-9]{1,3}\.){3}[0-9]{1,3}([^[:alnum:].]|$)'
    local ipv6='(^|[^[:alnum:]:])([0-9A-Fa-f]{1,4}:){7}[0-9A-Fa-f]{1,4}([^[:alnum:]:]|$)'
    local short6='(^|[^[:alnum:]:])(([0-9A-Fa-f]{1,4}:)+:([0-9A-Fa-f]{1,4}(:[0-9A-Fa-f]{1,4})*)?|::[0-9A-Fa-f]{1,4}(:[0-9A-Fa-f]{1,4})*)([^[:alnum:]:]|$)'
    rules+=(-e "s/$ipv4/\1<ip>\3/g" -e "s/$ipv4/\1<ip>\3/g" -e "s/$ipv6/\1<ip>\3/g" -e "s/$short6/\1<ip>\7/g"
        -e "s/$short6/\1<ip>\7/g"
        -e 's/(^|[^[:alnum:]_-])(sk-|ghp_|gho_|github_pat_|xox[abpr]-|AKIA)[A-Za-z0-9_-]{8,}/\1<token>/g')
    # Last, any run of 24 or more letters, digits, _ and - that mixes letters and digits.
    LC_ALL=C sed -E "${rules[@]}" | LC_ALL=C awk '{
        out = ""; rest = $0
        while (match(rest, /[A-Za-z0-9_-]+/)) {
            word = substr(rest, RSTART, RLENGTH)
            if (RLENGTH >= 24 && word ~ /[A-Za-z]/ && word ~ /[0-9]/) word = "<token>"
            out = out substr(rest, 1, RSTART - 1) word
            rest = substr(rest, RSTART + RLENGTH)
        }
        print out rest
    }'
}

ere_escape() { printf '%s\n' "$1" | sed 's/[]\/$*.^|[(){}?+]/\\&/g'; }  # TEXT as a sed -E pattern

# urlencode TEXT: percent-encoded UTF-8; RFC 3986's unreserved characters stay as they are. A subshell, so
# that LC_ALL=C (a byte at a time) ends with it.
urlencode() (
    LC_ALL=C
    text=$1 out='' i=0
    while [ "$i" -lt "${#text}" ]; do
        c=${text:i:1}
        case $c in
            [A-Za-z0-9._~-]) out=$out$c ;;
            *)
                printf -v n '%d' "'$c"
                if [ "$n" -lt 0 ]; then n=$((n + 256)); fi  # bash 3.2 reads bytes over 127 as signed
                printf -v c '%%%02X' "$n"
                out=$out$c
                ;;
        esac
        i=$((i + 1))
    done
    printf '%s' "$out"
)

report_environment() {  # what the bug template asks about the machine
    local version tmux_v=missing
    version=$(ui_version)
    if has tmux; then tmux_v=$(tmux -V 2>/dev/null | sed 's/^tmux //'); fi
    printf 'tmux-workspace: %s (installing)\n' "${version:-${WANTED:-the latest release}}"
    printf 'OS: %s\narch: %s\nbash: %s\n' "$(os_label full)" "$(uname -m)" "$BASH_VERSION"
    printf 'package manager: %s\ntmux: %s\n' "${PM:-none}" "${tmux_v:-missing}"
}

# report_link STEP: a new issue of the bug template, its title naming STEP, with the machine and the last
# 20 lines of the log. A link stays within 8000 bytes: the oldest lines of the log go first.
report_link() {
    local base title environment line encoded=() n start=0 total=0 i diagnostics=''
    title=$(printf 'install failed: %s\n' "$1" | anonymize)
    environment=$(report_environment | anonymize)
    base="$REPO_URL/issues/new?template=bug.yml&title=$(urlencode "$title")&environment=$(urlencode "$environment")&diagnostics="
    while IFS= read -r line; do
        encoded[${#encoded[@]}]=$(urlencode "$line")
        total=$((total + ${#encoded[${#encoded[@]} - 1]} + 3))
    done < <(log_tail 20 | anonymize)
    n=${#encoded[@]}
    total=$((total - 3))
    while [ "$start" -lt "$n" ] && [ $((${#base} + total)) -gt 8000 ]; do
        total=$((total - ${#encoded[start]} - 3))
        start=$((start + 1))
    done
    for ((i = start; i < n; i++)); do
        if [ "$i" -gt "$start" ]; then diagnostics=$diagnostics%0A; fi
        diagnostics=$diagnostics${encoded[i]}
    done
    printf '%s%s\n' "$base" "$diagnostics"
}

copy_report() {  # the anonymized end of the log on the clipboard too, where there is one
    local copied=1
    if has pbcopy; then
        log_tail 20 | anonymize | pbcopy 2>/dev/null || copied=0
    elif has wl-copy && [ -n "${WAYLAND_DISPLAY:-}" ]; then
        log_tail 20 | anonymize | wl-copy 2>/dev/null || copied=0
    elif has xclip && [ -n "${DISPLAY:-}" ]; then
        log_tail 20 | anonymize | xclip -selection clipboard 2>/dev/null || copied=0
    else
        return 0
    fi
    if [ "$copied" = 1 ]; then ui '  The end of the log, anonymized, is on the clipboard as well.'; fi
}

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
    pm_install "$@"
}

pm_install() {  # pm_install PACKAGE...: the package manager installs them
    case $PM in
        brew) brew install "$@" ;;
        apt-get) DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq "$@" ;;
        dnf) $SUDO dnf install -y -q "$@" ;;
        pacman) $SUDO pacman -S --needed --noconfirm "$@" ;;
        apk) $SUDO apk add -q "$@" ;;
        zypper) $SUDO zypper install -y "$@" ;;
    esac
}

pm_usable() { [ -n "$PM" ] && { [ -z "$SUDO" ] || has sudo; }; }  # packages can be installed here

pm_has() {  # pm_has TOOL: the package manager here has a package of TOOL
    case $1 in
        sesh) [ "$PM" = brew ] ;;
        lazygit) case $PM in brew | pacman | apk) return 0 ;; *) return 1 ;; esac ;;
        procps) [ "$PM" = apk ] ;;
        *) [ -n "$PM" ] ;;
    esac
}

package_of() {  # package_of TOOL: the name of its package here
    if [ "$1" = python3 ] && [ "$PM" = pacman ]; then echo python; else echo "$1"; fi
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

# try_fetch URL DEST: download with curl or wget; a URL without a scheme is a local file. When that
# fails it returns 1, with the reason in REASON.
try_fetch() {
    local url=$1 dest=$2
    REASON_KIND=failed
    case $url in
        *://*) ;;
        *)
            cp "$url" "$dest" 2>/dev/null && return 0
            REASON="could not read $url"
            return 1
            ;;
    esac
    if ! has curl && ! has wget && [ "$DEPS" = 1 ] && [ "$DRY" = 0 ]; then install_packages curl; fi
    if has curl; then
        curl -fsSL --retry 2 -o "$dest" "$url" && return 0
    elif has wget; then
        wget -q -O "$dest" "$url" && return 0
    else
        REASON="need curl or wget to download tmux-workspace"
        return 1
    fi
    REASON="could not download $url"
    return 1
}

fetch() { try_fetch "$1" "$2" || die "$REASON" 3; }  # fetch URL DEST, or die saying why

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

# try_stage_archive TAG: download this machine's archive of release TAG (vX.Y.Z or latest) and the
# release's SHA256SUMS to a temporary folder, check the one against the other, and unpack it, owned
# by whoever runs this. Sets STAGE, the folder (removed on exit), and STAGED, the release in it;
# writes nothing else. When no release fits this machine (REASON_KIND missing) or the download fails,
# it returns 1 with the reason in REASON. A checksum that does not match dies: that is no reason to
# try another way.
try_stage_archive() {
    local target name dir base
    REASON='' REASON_KIND=failed
    target=$(detect_target)
    if [ -z "$target" ]; then
        REASON="no prebuilt binary for $(uname -s)/$(uname -m)"
        REASON_KIND=missing
        return 1
    fi
    if ! has tar; then
        REASON="need tar to unpack tmux-workspace"
        return 1
    fi
    base=$(dist_base "$1")
    name=tmux-workspace-$target.tar.gz
    remove_stage
    STAGE=$(mktemp -d) || { REASON='could not make a temporary folder'; return 1; }
    say download "$base/$name"
    if ! try_fetch "$base/$name" "$STAGE/$name" || ! try_fetch "$base/SHA256SUMS" "$STAGE/SHA256SUMS"; then
        remove_stage
        return 1
    fi
    verify "$STAGE/$name" "$STAGE/SHA256SUMS"
    mkdir "$STAGE/release"
    tar --no-same-owner -xzf "$STAGE/$name" -C "$STAGE/release" || die "could not unpack $name" 3
    rm -f "${STAGE:?}/${name:?}"
    STAGED=''
    for dir in "$STAGE"/release/tmux-workspace-*; do
        if [ -f "$dir/VERSION" ] && [ -f "$dir/install.sh" ] && [ -x "$dir/bin/tmux-workspace" ]; then STAGED=$dir; fi
    done
    [ -n "$STAGED" ] || die "$name holds no tmux-workspace release" 3
}

stage_archive() {  # stage_archive TAG: try_stage_archive, or die saying why and what to do
    try_stage_archive "$1" && return 0
    if [ "$REASON_KIND" = missing ]; then
        if has cargo; then
            die "$REASON; clone $REPO_URL and run its install.sh, which builds one with cargo" 3
        fi
        die "$REASON; install Rust (https://rustup.rs) and rerun" 3
    fi
    die "$REASON" 3
}

# run_staged: hand over to the installer of the release just downloaded, with the same options. It
# copies its release next to the other versions, links from there, and removes the download.
run_staged() {
    TMUX_WORKSPACE_HOME=${HOME_SHARE:-$(share_home)}
    TMUX_WORKSPACE_STAGE=$STAGE
    export TMUX_WORKSPACE_HOME TMUX_WORKSPACE_STAGE
    progress_close
    exec bash "$STAGED/install.sh" ${ORIGINAL_ARGS[@]+"${ORIGINAL_ARGS[@]}"}
}

# Piped, with a clone at the install home: the piped install of v1 cloned there. Rather than unpack a
# release beside it, update the clone in place (fast-forward only, the new binary first), and let
# the installer that came with the pull do the rest. Hook entries of v1 keep working through
# integrations/agent-hook.py, which runs the binary now.
clone_install() {
    local root=$1
    if [ "$UNINSTALL" = 0 ]; then
        git_update "$root"
        if [ "$UPDATE" = 1 ] && [ -z "$UPDATED_FROM" ]; then
            say "done" "already up to date ($(clone_label "$root" HEAD))"
            exit 0
        fi
        if [ -n "$UPDATED_FROM" ]; then
            TMUX_WORKSPACE_UPDATE_FROM=$UPDATED_FROM
            export TMUX_WORKSPACE_UPDATE_FROM
        elif [ -f "$root/Cargo.toml" ] && [ "$DRY" = 0 ]; then
            clone_release_binary "$root"
        fi
    fi
    progress_close
    exec bash "$root/install.sh" ${ORIGINAL_ARGS[@]+"${ORIGINAL_ARGS[@]}"}
}

# clone_release_binary ROOT: a piped run over a clone that is up to date. Unless bin/ has the release
# binary of the clone's version already, get it (cargo only when no release fits), and have the
# clone's installer take it rather than build one.
clone_release_binary() {
    local root=$1 dest=$1/bin/tmux-workspace stamp=$1/bin/tmux-workspace.version
    if [ ! -x "$dest" ] || [ "$(cat "$stamp" 2>/dev/null || true)" != "$(tree_version "$root")" ]; then
        step binary "$(ui_path "$dest")" slow
        obtain_binary "$root" '' "release cargo"
        place_binary "$NEW_BINARY" "$dest" "$stamp" "$NEW_STAMP"
        remove_stage
        say binary "$dest ($(binary_origin))"
    fi
    TMUX_WORKSPACE_BINARY=$dest
    export TMUX_WORKSPACE_BINARY
}

# Piped (`curl … | bash`): no clone or release is around this script. Download the release, and its
# own installer does the rest.
remote_install() {
    local home
    home=$(share_home)
    detect_platform
    if [ -e "$home/.git" ]; then clone_install "$home"; fi
    # Removing or updating is the business of the installed release's own installer.
    if [ "$UNINSTALL" = 1 ] || [ "$UPDATE" = 1 ]; then
        if [ -f "$home/current/install.sh" ]; then
            exec bash "$home/current/install.sh" ${ORIGINAL_ARGS[@]+"${ORIGINAL_ARGS[@]}"}
        fi
        [ "$UPDATE" = 0 ] || die "no tmux-workspace in $home, so nothing to update; install it without --update" 1
        say "done" "nothing to remove"
        exit 0
    fi
    PIPED=1
    if [ "$UI" = guided ]; then
        # Without tmux and a way to install it, say so before downloading. Until the checklist is
        # answered nothing is written, the log of the download included.
        tmux_usable || pm_usable || stop_without_tmux "$(tmux_version 2>/dev/null || true)"
        PENDING_LOG=$(mktemp)
    fi
    step download "tmux-workspace ${WANTED:-(the latest release)}" slow
    stage_archive "${WANTED:-latest}"
    if [ "$UI" != plain ]; then
        STEP=$(printf '%-22s%s' download "tmux-workspace $(release_version "$STAGED"), checked against SHA256SUMS")
    fi
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
        PIPED=1
    elif [ -L "$parent/current" ]; then
        HOME_SHARE=$parent
        if [ -n "$WANTED" ] && [ "${WANTED#v}" != "$(release_version "$REPO")" ] && [ "$UNINSTALL$UPDATE" = 00 ]; then
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
    step unpack "tmux-workspace $own to $(ui_path "$home")"
    mkdir -p "$home"
    if [ ! -f "$dest/VERSION" ] || [ ! -x "$dest/bin/tmux-workspace" ]; then
        rm -rf "${home:?}/.incoming-$$" "${home:?}/.outgoing-$$"
        cp -R "$REPO" "$home/.incoming-$$"  # owned by whoever runs this, not by the download
        if [ -e "$dest" ] || [ -L "$dest" ]; then mv "$dest" "$home/.outgoing-$$"; fi
        mv "$home/.incoming-$$" "$dest"
        rm -rf "${home:?}/.outgoing-$$"
        changed
        say unpack "tmux-workspace $own to $dest"
    fi
    previous=$(readlink "$home/current" 2>/dev/null || true)
    if [ "$previous" != "$own" ]; then
        ln -sfn "$own" "$home/current"
        changed
        say link "$home/current -> $own"
        prune_releases "$home" "$own" "$previous"  # the new current and the one before it stay
    fi
    REPO=$home/current
}

# is_release DIR NAME: DIR is an unpacked release of version NAME, and no link: all that pruning removes.
is_release() {
    [ ! -L "$1" ] && [ -d "$1" ] && [ ! -L "$1/bin" ] && [ -f "$1/bin/tmux-workspace" ] && [ -f "$1/install.sh" ] &&
        [ -f "$1/VERSION" ] && [ "$(release_version "$1")" = "$2" ]
}

# prune_releases HOME KEEP...: remove the other unpacked versions in HOME, and the copies that an
# interrupted run left behind. Nothing else: HOME may hold other things.
prune_releases() {
    local home=$1 dir name pid
    shift
    for dir in "$home"/[0-9]*.[0-9]*.[0-9]*; do
        name=${dir##*/}
        printf '%s\n' "$name" | grep -Eq "^$VERSION_RE\$" || continue
        is_release "$dir" "$name" || continue
        case " $* " in *" $name "*) continue ;; esac
        rm -rf "${dir:?}"
        say remove "$dir"
    done
    for dir in "$home"/.incoming-* "$home"/.outgoing-*; do
        pid=${dir##*-}
        case $pid in '' | *[!0-9]*) continue ;; esac
        if [ ! -f "$dir/VERSION" ] || ! is_release "$dir" "$(release_version "$dir")"; then continue; fi
        if kill -0 "$pid" 2>/dev/null; then continue; fi  # still copying
        rm -rf "${dir:?}"
    done
}

# ---------------------------------------------------------------------------------------------
# Updates (--update): the installer of the new version does the install, so its fixes apply too.

latest_version() {  # sets LATEST, the version of the latest release, from the VERSION file it carries
    remove_stage
    STAGE=$(mktemp -d)
    fetch "$(dist_base latest)/VERSION" "$STAGE/VERSION"
    LATEST=$(head -n 1 "$STAGE/VERSION" | tr -d '[:space:]')
    remove_stage
    case $LATEST in
        [0-9]*.[0-9]*.[0-9]*) ;;
        *) die "could not tell the latest version from $(dist_base latest)/VERSION" 3 ;;
    esac
}

# --update of an installed release: when the wanted release (the latest, or --version) is not the
# current one, download it and hand over to its installer, which installs it with the choices made
# before. The latest release only ever replaces an older one. --check only says.
update_release() {
    local parent own wanted
    parent=$(dirname "$REPO")
    if [ ! -L "$parent/current" ]; then
        die "$REPO is not an installed release; run its install.sh without --update to install it" 1
    fi
    HOME_SHARE=$parent
    own=$(release_version "$parent/current")
    if [ -n "$WANTED" ]; then
        wanted=${WANTED#v}
    else
        step 'latest release' 'asking which version is the newest' slow
        latest_version
        if [ "$UI" != plain ]; then STEP=$(printf '%-22s%s' 'latest release' "$LATEST"); fi
        wanted=$LATEST
        version_lt "$own" "$wanted" || wanted=$own
    fi
    if [ "$wanted" = "$own" ]; then
        say "done" "already up to date ($own)"
        ui_done "tmux-workspace $own is up to date."
        exit 0
    fi
    if [ "$CHECK" = 1 ]; then
        say update "update available: $own → $wanted"
        exit 0
    fi
    step download "tmux-workspace $wanted" slow
    stage_archive "v$wanted"
    TMUX_WORKSPACE_UPDATE_FROM=$own
    export TMUX_WORKSPACE_UPDATE_FROM
    run_staged
}

# --update of a clone: fast-forward it to its upstream, then hand over to the installer that came
# with the pull, with the choices made before.
update_clone() {
    [ -e "$REPO/.git" ] || die "$REPO is neither a git clone nor an installed release; there is nothing to update" 1
    git_update "$REPO"
    if [ -z "$UPDATED_FROM" ]; then
        say "done" "already up to date ($(clone_label "$REPO" HEAD))"
        ui_done "tmux-workspace $(clone_label "$REPO" HEAD) is up to date."
        exit 0
    fi
    TMUX_WORKSPACE_UPDATE_FROM=$UPDATED_FROM
    export TMUX_WORKSPACE_UPDATE_FROM
    progress_close
    exec bash "$REPO/install.sh" ${ORIGINAL_ARGS[@]+"${ORIGINAL_ARGS[@]}"}
}

# git_update ROOT: fast-forward the clone at ROOT to its upstream. A clone with uncommitted changes, on
# another branch than the default one, or with commits of its own is left alone. The binary of the
# new version is had before the pull and moved into bin/ right after it, so that
# ~/.local/bin/tmux-workspace never leads to a missing file. Sets UPDATED_FROM to the version it
# updated from, or to nothing when the clone was up to date. --check only says.
git_update() {
    local root=$1 branch default from to upstream dirty=0 elsewhere=0
    UPDATED_FROM=''
    has git || die "git is needed to update the clone in $root" 3
    [ -z "$(git -C "$root" status --porcelain --untracked-files=no)" ] || dirty=1
    branch=$(git -C "$root" symbolic-ref --quiet --short HEAD || true)
    default=$(git -C "$root" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)
    default=${default#origin/}
    default=${default:-main}
    [ "$branch" = "$default" ] || elsewhere=1
    if [ "$CHECK" = 0 ]; then
        [ "$dirty" = 0 ] || die "$root has uncommitted changes; commit or stash them, then update again" 1
        if [ "$elsewhere" = 1 ]; then
            die "$root is on ${branch:-a detached HEAD}, not $default; switch to $default to update it" 1
        fi
    fi
    step updates "fetching them for $(ui_path "$root")" slow
    git -C "$root" fetch --quiet || die "could not fetch the updates of $root" 3
    upstream='@{upstream}'
    [ "$elsewhere" = 0 ] || upstream=origin/$default  # --check only: what an update on $default would get
    if ! git -C "$root" rev-parse --verify --quiet "$upstream" >/dev/null; then
        die "$default in $root follows no remote branch; there is nothing to update from" 1
    fi
    if [ "$CHECK" = 1 ]; then
        [ "$dirty" = 0 ] || say note "$root has uncommitted changes, which an update refuses"
        [ "$elsewhere" = 0 ] || say note "$root is on ${branch:-a detached HEAD}, not $default; an update needs $default"
    fi
    from=$(clone_label "$root" HEAD)
    [ "$(git -C "$root" rev-parse HEAD)" != "$(git -C "$root" rev-parse "$upstream")" ] || return 0
    if ! git -C "$root" merge-base --is-ancestor HEAD "$upstream"; then
        [ "$CHECK" = 1 ] || die "$root has commits that are not upstream; push or drop them, then update again" 1
        say note "$root has commits that are not upstream, which an update refuses"
    fi
    to=$(clone_label "$root" "$upstream")
    if [ "$CHECK" = 1 ]; then
        say update "update available: $from → $to"
        exit 0
    fi
    UPDATED_FROM=$from
    if [ "$DRY" = 1 ]; then
        say would "update $root from $from to $to"
        return 0
    fi
    # The new binary goes next to the old one before the pull, so after it only a rename is left.
    step 'new binary' "of $to" slow
    obtain_binary "$root" "$upstream" "release cargo"
    mkdir -p "$root/bin"
    PENDING=$root/bin/tmux-workspace.new.$$
    { cp "$NEW_BINARY" "$PENDING" && chmod 755 "$PENDING"; } || die "could not write $PENDING; $root is unchanged" 3
    remove_stage
    step pull "$(ui_path "$root") to $to"
    git -C "$root" merge --ff-only --quiet "$upstream" || die "could not fast-forward $root" 1
    changed
    mv -f "$PENDING" "$root/bin/tmux-workspace"
    PENDING=''
    if [ -n "$NEW_STAMP" ]; then
        printf '%s\n' "$NEW_STAMP" >"$root/bin/tmux-workspace.version"
    else
        rm -f "${root:?}/bin/tmux-workspace.version"
    fi
    say update "$root from $from to $to"
    TMUX_WORKSPACE_BINARY=$root/bin/tmux-workspace
    export TMUX_WORKSPACE_BINARY
}

# obtain_binary ROOT REF ORDER: the binary of the clone at ROOT, for its working tree (REF empty) or
# for REF before it is pulled, had without changing the clone. TMUX_WORKSPACE_BINARY when it is set;
# else the ways in ORDER ("cargo release" or "release cargo"), each tried when the one before fails:
# the release of that version (downloaded and checked) or a build with cargo. Sets NEW_BINARY, and
# NEW_STAMP to the release it came from. When no way works it dies, saying what to do.
obtain_binary() {
    local root=$1 ref=$2 order=$3 version way release_reason='' release_kind='' cargo=''
    NEW_BINARY='' NEW_STAMP=''
    if [ -n "${TMUX_WORKSPACE_BINARY:-}" ]; then
        [ -f "$TMUX_WORKSPACE_BINARY" ] || die "TMUX_WORKSPACE_BINARY is not a file: $TMUX_WORKSPACE_BINARY" 3
        NEW_BINARY=$TMUX_WORKSPACE_BINARY
        return 0
    fi
    if [ -z "$ref" ]; then version=$(tree_version "$root"); else version=$(ref_version "$root" "$ref"); fi
    for way in $order; do
        if [ "$way" = release ]; then
            if [ -z "$version" ]; then
                release_reason="no version in Cargo.toml to name its release" release_kind=missing
            elif try_stage_archive "v$version"; then
                NEW_BINARY=$STAGED/bin/tmux-workspace
                NEW_STAMP=$version
                if [ "$cargo" = failed ]; then say note "cargo could not build it; this is the release of $version"; fi
                return 0
            else
                release_reason=$REASON release_kind=$REASON_KIND
            fi
        elif ! has cargo; then
            cargo=missing
        elif cargo_build "$root" "$ref"; then
            return 0
        else
            cargo=failed
        fi
    done
    if [ "$cargo" = failed ]; then
        die "$release_reason, and cargo could not build it (that takes Rust $RUST_MIN or newer from https://rustup.rs and a C compiler); $root is unchanged" 3
    elif [ "$release_kind" = missing ]; then
        die "$release_reason; install Rust (https://rustup.rs) and rerun" 3
    fi
    die "$release_reason; check the connection and rerun, or install Rust (https://rustup.rs) to build it" 3
}

# cargo_build ROOT REF: build the clone at ROOT with cargo, into ROOT/target: its working tree in
# place (REF empty), or REF exported with git archive to a temporary folder. Sets NEW_BINARY to a
# copy of the result; returns 1 when cargo fails.
cargo_build() {
    local root=$1 ref=$2 src=$1 target_dir
    target_dir=${CARGO_TARGET_DIR:-$1/target}
    remove_stage
    STAGE=$(mktemp -d) || return 1
    if [ -n "$ref" ]; then
        src=$STAGE/src
        mkdir "$src"
        git -C "$root" archive "$ref" | tar -x -C "$src" || die "could not read the new version of $root" 3
        say build "cargo build --release --locked, for the new version"
    else
        say build "cargo build --release --locked"
    fi
    (cd "$src" && CARGO_TARGET_DIR=$target_dir cargo build --release --locked) || return 1
    [ -f "$target_dir/release/tmux-workspace" ] || return 1
    cp "$target_dir/release/tmux-workspace" "$STAGE/tmux-workspace" || return 1
    NEW_BINARY=$STAGE/tmux-workspace
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

binary_origin() { if [ -n "$NEW_STAMP" ]; then echo "release $NEW_STAMP"; else echo built; fi; }

# install_binary: the binary of this clone into bin/. TMUX_WORKSPACE_BINARY, else built with cargo,
# else (or when cargo cannot build it) the release of the clone's version, which a later run without
# cargo does not download again.
install_binary() {
    local dest=$REPO/bin/tmux-workspace stamp=$REPO/bin/tmux-workspace.version given=${TMUX_WORKSPACE_BINARY:-}
    if [ -n "$given" ]; then
        [ -f "$given" ] || die "TMUX_WORKSPACE_BINARY is not a file: $given" 3
        if [ "$(resolve "$given")" = "$(resolve "$dest")" ] || cmp -s "$given" "$dest" 2>/dev/null; then return 0; fi
        step binary "$(ui_path "$dest") from $(ui_path "$given")"
        act binary "$dest (from $given)" place_binary "$given" "$dest" "$stamp" ''
        return 0
    fi
    if ! has cargo && [ -x "$dest" ] && [ "$(cat "$stamp" 2>/dev/null || true)" = "$(tree_version "$REPO")" ]; then
        return 0
    fi
    if [ "$DRY" = 1 ]; then
        say would "build $dest with cargo, or download the release of $(tree_version "$REPO")"
        return 0
    fi
    if has cargo; then
        step build 'cargo build --release --locked' slow
    else
        step download "tmux-workspace $(tree_version "$REPO")" slow
    fi
    obtain_binary "$REPO" '' "cargo release"
    if [ -n "$NEW_STAMP" ] && has cargo; then STEP_NOTE="cargo could not build it; this is the release of $NEW_STAMP"; fi
    if cmp -s "$NEW_BINARY" "$dest" 2>/dev/null && [ "$(cat "$stamp" 2>/dev/null || true)" = "$NEW_STAMP" ]; then
        remove_stage
        return 0
    fi
    act binary "$dest ($(binary_origin))" \
        place_binary "$NEW_BINARY" "$dest" "$stamp" "$NEW_STAMP"
    remove_stage
}

# ---------------------------------------------------------------------------------------------
# Requirements

ok() { [ "$UPDATE" = 1 ] || say ok "$1"; }

# Every install checks this before it changes anything. On a terminal, that is where the checklist asks
# what to set up, and installs the tools it was given.
check_requirements() {
    [ "$CHECKED" = 0 ] || return 0
    CHECKED=1
    if [ "$UI" = guided ]; then
        guided_setup
        return 0
    fi
    if [ "$UI" != plain ]; then step requirements "tmux $(tmux -V 2>/dev/null | sed 's/^tmux //')" quick; fi
    local missing='' old='' tool version
    if has tmux; then
        version=$(tmux_version)
        if [ -n "$version" ] && ! version_ge "$version" "$TMUX_MIN"; then
            old="tmux $version is older than $TMUX_MIN"
        fi
        ok "tmux $(tmux -V 2>/dev/null | sed 's/^tmux //')"
    else
        missing="$missing tmux"
    fi
    # Only the commands not ported from Python yet need it (stages 2 and 3 of the Rust port).
    version=$(python_version)
    if [ -n "$version" ] && version_ge "$version" "$PYTHON_MIN"; then
        ok "python3 $version"
    else
        say warn "python3 $PYTHON_MIN+ not found: dash, ai and cleanup need it until they are ported"
    fi
    local optional='' recommended='' upgrade=''
    for tool in fzf zoxide git sesh lazygit; do
        if has "$tool"; then ok "$tool"; else optional="$optional $tool"; fi
    done
    for tool in fzf zoxide; do
        has "$tool" || recommended="$recommended $tool"
    done
    # Alpine's ps is BusyBox's, which lacks the options agent status and snapshots read processes with.
    if [ "$PM" = apk ] && ! ps -p $$ -o pid=,tpgid=,lstart= >/dev/null 2>&1; then
        optional="$optional procps"
        recommended="$recommended procps"
    fi
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
    [ "$UPDATE" = 0 ] || optional=''  # an update says only what is wrong
    for tool in $optional; do
        case $tool in
            fzf) say optional "fzf: fuzzy workspace and tab pickers ($(install_command fzf))" ;;
            zoxide) say optional "zoxide: \`work\` finds folders you visit often" ;;
            git) say optional "git: lazygit and updating a clone use it" ;;
            sesh) say optional "sesh: picker fallback without fzf" ;;
            lazygit) say optional "lazygit: Ctrl+a g opens it in a popup" ;;
            procps) say optional "procps: agent status and snapshots need its ps ($(install_command procps))" ;;
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
# The checklist: what to set up, a row each. Needed rows are always set up; recommended rows start
# ticked and optional ones not. Arrow keys or j and k move, space ticks, Enter installs, q or Esc quits.

os_label() {  # os_label [full]: macOS 27 or Debian 12; full: macOS 27.2, Debian GNU/Linux 12 (bookworm)
    local name='' version=''
    case $(uname -s) in
        Darwin)
            if has sw_vers; then version=$(sw_vers -productVersion 2>/dev/null || true); fi
            [ "${1:-}" = full ] || version=${version%%.*}
            printf 'macOS%s\n' "${version:+ $version}"
            ;;
        Linux)
            if [ -r /etc/os-release ]; then
                if [ "${1:-}" = full ]; then name=$(os_release PRETTY_NAME); fi
                if [ -z "$name" ]; then
                    name=$(os_release NAME)
                    name=${name% GNU/Linux}
                    name=${name% Linux}
                    version=$(os_release VERSION_ID)
                fi
            fi
            printf '%s%s\n' "${name:-Linux}" "${version:+ $version}"
            ;;
        *) uname -s ;;
    esac
}

os_release() { sed -n "s/^$1=//p" /etc/os-release | head -n 1 | tr -d '"'; }

codex_home() { printf '%s\n' "${CODEX_HOME:-$HOME/.codex}"; }  # where Codex keeps hooks.json

shell_targets() {  # where the work command goes: the files shell_rc and the fish link change
    local list='' rc=${ZDOTDIR:-$HOME}/.zshrc shell
    shell=$(basename "${SHELL:-sh}")
    if [ -e "$rc" ] || [ "$shell" = zsh ]; then list=$(ui_path "$rc"); fi
    if [ -e "$HOME/.bashrc" ] || [ "$shell" = bash ]; then list="${list:+$list, }$(ui_path "$HOME/.bashrc")"; fi
    if has fish || [ -d "$HOME_CONFIG/fish" ]; then list="${list:+$list, }fish"; fi
    printf '%s\n' "$list"
}

agent_names() {  # the agents that `integrations install` registers here
    local names=''
    if [ -d "$HOME/.claude" ]; then names='Claude Code'; fi
    if [ -d "$(codex_home)" ]; then names="${names:+$names, }Codex"; fi
    if [ -d "$HOME/.omp" ]; then names="${names:+$names, }omp"; fi
    if [ -d "$HOME/.config/nvim" ]; then names="${names:+$names, }Neovim"; fi
    if [ "$(uname -s)" = Darwin ]; then names="${names:+$names, }iTerm2 profile"; fi
    printf '%s\n' "${names:-none found}"
}

# The rows: R_KIND need, rec or opt; R_ON ticked; R_PKG the package a row installs; R_AGENT 1 for the
# agent rows. One `integrations install` registers every agent, so those go on and off together.
NROWS=0 NAGENTS=0 CURSOR=-1 ANSWERED=0 DRAWN=0 FOUND='' NOTFOUND='' HEADER=''
R_KIND=() R_NAME=() R_HINT=() R_ON=() R_PKG=() R_AGENT=() SCREEN=()

add_row() {  # add_row KIND NAME HINT [ON [PACKAGE [AGENT]]]
    R_KIND[NROWS]=$1 R_NAME[NROWS]=$2 R_HINT[NROWS]=$3 R_ON[NROWS]=${4:-1} R_PKG[NROWS]=${5:-} R_AGENT[NROWS]=${6:-0}
    NROWS=$((NROWS + 1))
}

found() { FOUND="${FOUND:+$FOUND, }$1"; }

where_to_get() {  # where_to_get TOOL: for a tool no package here installs
    case $1 in
        fzf) echo github.com/junegunn/fzf ;;
        zoxide) echo github.com/ajeetdsouza/zoxide ;;
        lazygit) echo github.com/jesseduffield/lazygit ;;
        sesh) echo github.com/joshmedeski/sesh ;;
        git) echo git-scm.com ;;
        python3) echo python.org ;;
        *) install_command "$1" ;;
    esac
}

# offer KIND TOOL [HAVE]: a row that installs TOOL with the package manager, or, when that cannot, TOOL on
# the "not found" line with where to get it. HAVE is the version there is, when it is too old.
offer() {
    local kind=$1 tool=$2 name=$2 why='' on=0 package
    case $tool in
        fzf) why='fuzzy pickers' ;;
        zoxide) why="\`work\` finds folders you visit often" ;;
        python3) why="dash, ai and cleanup need it${3:+ (this one is $3)}" name="python3 $PYTHON_MIN+" ;;
        procps) why='agent status and snapshots need its ps' ;;
        lazygit) why='Ctrl+a g popup' ;;
        sesh) why='picker fallback' ;;
        git) why='lazygit and updating a clone use it' ;;
    esac
    if [ "$kind" = rec ]; then on=1; fi
    if pm_usable && pm_has "$tool"; then
        package=$(package_of "$tool")
        add_row "$kind" "$name" "$why $G_DOT $(install_command "$package")" "$on" "$package"
    else
        NOTFOUND="$NOTFOUND$tool ($(where_to_get "$tool"))"$'\n'
    fi
}

# No tmux 3.3+ and nothing to install it with: say what to run, and stop before the checklist.
stop_without_tmux() {
    printf '%s\n' "tmux-workspace needs tmux $TMUX_MIN or newer${1:+ (this one is $1)}, and no package manager here can install it."
    if [ "$(uname -s)" = Darwin ]; then
        printf '  Install Homebrew (https://brew.sh), then run:\n    brew install tmux\n'
    elif [ -n "$PM" ]; then
        printf '  As root, run:\n    %s\n' "$(SUDO='' install_command tmux)"
    else
        printf '  Install tmux %s or newer with the package manager of your system.\n' "$TMUX_MIN"
    fi
    printf '  Then run this installer again:\n    %s\n' "$(ui_rerun)"
    exit 3
}

tmux_usable() {  # tmux is there, and new enough
    local version
    has tmux || return 1
    version=$(tmux_version)
    [ -z "$version" ] || version_ge "$version" "$TMUX_MIN"
}

survey() {  # the rows of this machine, and the tools it has
    local tool version python='' hint
    for tool in tmux fzf zoxide git lazygit sesh; do
        if [ "$tool" = tmux ]; then
            if tmux_usable; then found "tmux $(tmux -V 2>/dev/null | sed 's/^tmux //')"; fi
        elif has "$tool"; then
            found "$tool"
        fi
    done
    version=$(python_version)
    if [ -n "$version" ] && version_ge "$version" "$PYTHON_MIN"; then
        found "python3 $version"
    else
        python="old $version"  # missing, or too old: a row
    fi
    HEADER="$(os_label) $(uname -m) $G_DOT $(basename "${SHELL:-sh}") $G_DOT ${PM:-no package manager}"
    HEADER="$HEADER${FOUND:+ $G_DOT found: $FOUND}"

    add_row need tmux-workspace "$(ui_path "$BIN_DIR/tmux-workspace")"
    if ! tmux_usable; then
        if has tmux; then version=$(tmux_version); else version=''; fi
        pm_usable || stop_without_tmux "$version"
        add_row need "tmux $TMUX_MIN+" "${version:+this one is $version $G_DOT }$(install_command tmux)" 1 tmux
    fi
    if [ "$TMUXCONF" = 1 ]; then
        hint=$(ui_path "$HOME/.tmux.conf")
        if { [ -e "$HOME/.tmux.conf" ] || [ -L "$HOME/.tmux.conf" ]; } && ! installed_link "$HOME/.tmux.conf"; then
            hint="$hint (yours moves to a backup)"
        fi
        add_row need 'tmux config' "$hint"
    fi
    if [ "$SHELLS" = 1 ]; then
        hint=$(shell_targets)
        [ -z "$hint" ] || add_row need 'work command' "$hint"
    fi

    for tool in fzf zoxide; do has "$tool" || offer rec "$tool"; done
    if [ -n "$python" ]; then offer rec python3 "${python#old }"; fi
    if [ "$PM" = apk ] && ! ps -p $$ -o pid=,tpgid=,lstart= >/dev/null 2>&1; then offer rec procps; fi
    if [ -d "$HOME/.claude" ]; then add_row rec 'Claude Code status' "found $(ui_path "$HOME/.claude")" 1 '' 1; fi
    if [ -d "$(codex_home)" ]; then add_row rec 'Codex status' "found $(ui_path "$(codex_home)")" 1 '' 1; fi
    if [ -d "$HOME/.omp" ]; then add_row rec 'omp status' "found $(ui_path "$HOME/.omp")" 1 '' 1; fi
    if [ -d "$HOME/.config/nvim" ]; then add_row rec 'Neovim status' "found $(ui_path "$HOME/.config/nvim")" 1 '' 1; fi
    if [ "$(uname -s)" = Darwin ]; then add_row rec 'iTerm2 profile' "a \"tmux workspace\" profile" 1 '' 1; fi
    for tool in lazygit sesh git; do has "$tool" || offer opt "$tool"; done

    local i
    for ((i = 0; i < NROWS; i++)); do
        if [ "${R_AGENT[i]}" = 1 ]; then NAGENTS=$((NAGENTS + 1)); fi
        if [ "$CURSOR" -lt 0 ] && [ "${R_KIND[i]}" != need ]; then CURSOR=$i; fi
    done
}

# Whether PATH is a link into this installation: this clone or release, or any release installed.
installed_link() {
    [ -L "$1" ] || return 1
    case $(resolve "$1") in "$(resolve "$REPO")"/* | "$(resolve "$(share_home)")"/*) return 0 ;; esac
    return 1
}

screen_add() { SCREEN[${#SCREEN[@]}]=$1; }

wrap_items() {  # wrap_items PREFIX ITEMS: ITEMS (a line each) after PREFIX, wrapped to the terminal's width
    local line=$1 item indent
    indent=$(printf '%*s' "${#1}" '')
    while IFS= read -r item; do
        if [ -z "$item" ]; then continue; fi
        if [ "$line" = "$1" ]; then
            line=$line$item
        elif [ $((${#line} + ${#item} + 4)) -le "$COLS" ]; then
            line="$line, $item"
        else
            screen_add "$(fit "$line,")"
            line=$indent$item
        fi
    done <<EOF
$2
EOF
    screen_add "$(fit "$line")"
}

checklist_lines() {  # the checklist as it is now, a line each, in SCREEN
    local i kind='' box mark bold help
    SCREEN=()
    screen_add "$C_BOLD$(fit "tmux-workspace $(ui_version) installer")$C_OFF"
    screen_add "$(fit "$HEADER")"
    if [ -n "$NOTFOUND" ]; then wrap_items 'not found: ' "$NOTFOUND"; fi
    screen_add ''
    if [ "$ANSWERED" = 1 ]; then
        help=''  # the list stays on the terminal as a record of the choices
    elif [ "$CURSOR" -ge 0 ]; then
        help="$G_UPDOWN move $G_DOT space toggle $G_DOT enter install $G_DOT q quit"
    else
        help="enter install $G_DOT q quit"
    fi
    screen_add "${C_BOLD}What should I set up?$C_OFF   $C_DIM$(fit "$help" $((COLS - 25)))$C_OFF"
    screen_add ''
    for ((i = 0; i < NROWS; i++)); do
        if [ "${R_KIND[i]}" != "$kind" ]; then
            kind=${R_KIND[i]}
            case $kind in
                need) screen_add "${C_BOLD}Needed for tmux-workspace to work$C_OFF" ;;
                rec) screen_add "${C_BOLD}Recommended$C_OFF" ;;
                opt) screen_add "${C_BOLD}Optional$C_OFF" ;;
            esac
        fi
        if [ "$kind" = need ]; then
            box="[$C_DIM$G_LOCK$C_OFF]"
        elif [ "${R_ON[i]}" = 1 ]; then
            box="[${C_GREEN}x$C_OFF]"
        else
            box='[ ]'
        fi
        mark=' ' bold=''
        if [ "$i" = "$CURSOR" ]; then mark=$C_CYAN$G_CURSOR$C_OFF bold=$C_BOLD; fi
        screen_add "$mark $box $bold$(printf '%-22s' "${R_NAME[i]}")${bold:+$C_OFF}$C_DIM$(fit "${R_HINT[i]}" $((COLS - 29)))$C_OFF"
    done
    if [ "$NAGENTS" -ge 2 ]; then screen_add "${C_DIM}The agent rows go on and off together.$C_OFF"; fi
}

draw() {  # the checklist on the terminal, over the one drawn before
    local line
    checklist_lines
    if [ "$DRAWN" -gt 0 ]; then printf '\033[%dA' "$DRAWN"; fi
    for line in "${SCREEN[@]}"; do printf '\r\033[K%s\n' "$line"; done
    DRAWN=${#SCREEN[@]}
}

move() {  # move -1|1: the cursor to the row above or below that can be ticked
    local i=$CURSOR
    [ "$i" -ge 0 ] || return 0
    while :; do
        i=$((i + $1))
        if [ "$i" -lt 0 ] || [ "$i" -ge "$NROWS" ]; then return 0; fi
        if [ "${R_KIND[i]}" != need ]; then
            CURSOR=$i
            return 0
        fi
    done
}

toggle() {  # tick or untick the row of the cursor: every agent row at once
    local on i
    [ "$CURSOR" -ge 0 ] || return 0
    on=$((1 - R_ON[CURSOR]))
    if [ "${R_AGENT[CURSOR]}" = 1 ]; then
        for ((i = 0; i < NROWS; i++)); do
            if [ "${R_AGENT[i]}" = 1 ]; then R_ON[i]=$on; fi
        done
    else
        R_ON[CURSOR]=$on
    fi
}

# checklist: until Enter. q, Esc and Ctrl+D quit without a change; Ctrl+C too (the INT trap of choose_ui).
# The keys come from /dev/tty, never from standard input, which is the script itself when it is piped.
checklist() {
    local keys enter idle=0 second=$SECONDS
    exec 5</dev/tty
    TTY_SAVED=$(stty -g <&5)
    # Not canonical: a key at a time, without echo. A read gives up after 0.1 s, so that it returns the
    # bytes of an arrow key together and tells Esc from them. Ctrl+C still interrupts (isig).
    stty -icanon -echo min 0 time 1 <&5
    printf '\033[?25l'
    CURSOR_HIDDEN=1
    draw
    while :; do
        keys='' enter=0
        if IFS= read -r keys <&5; then enter=1; fi  # a read that ends with a newline: Enter
        if [ -z "$keys" ] && [ "$enter" = 0 ]; then
            # Nothing typed. A terminal that is gone returns at once instead, again and again.
            if [ "$SECONDS" = "$second" ]; then idle=$((idle + 1)); else idle=0 second=$SECONDS; fi
            [ "$idle" -lt 200 ] || die "lost the terminal; nothing was changed" 1
            continue
        fi
        while [ -n "$keys" ]; do
            case $keys in
                $'\033[A'* | $'\033OA'* | k*) move -1 ;;
                $'\033[B'* | $'\033OB'* | j*) move 1 ;;
                $'\033'[[O]*) ;;  # another key's escape sequence: ignored
                $'\033'* | q* | Q* | $'\004'*)
                    printf '\n%s\n' 'Nothing was changed.'
                    exit 0
                    ;;
                ' '*) toggle ;;
            esac
            case $keys in
                $'\033'[[O][AB]*) keys=${keys:3} ;;
                $'\033'[[O]*)
                    keys=${keys:2}
                    keys=${keys#*[A-Za-z~]}
                    ;;
                *) keys=${keys:1} ;;
            esac
        done
        [ "$enter" = 0 ] || break
        draw
    done
    CURSOR=-1 ANSWERED=1
    draw
    stty "$TTY_SAVED" <&5
    TTY_SAVED=''
    printf '\033[?25h'
    CURSOR_HIDDEN=0
    exec 5<&-
}

# The projects folder, inline, unless --projects or the config names it. Ctrl+D leaves it to the first
# `work`, as `tmux-workspace setup` does.
ask_projects() {
    local answer path
    [ -z "$PROJECTS" ] || return 0
    if grep -q '"projects_dir"' "$CONFIG_DIR/config.json" 2>/dev/null; then return 0; fi
    printf '\n'
    while :; do
        printf '%sProjects folder%s [~/Projects]: ' "$C_BOLD" "$C_OFF"
        if ! IFS= read -r answer </dev/tty; then
            printf '\n'
            return 0
        fi
        [ -n "$answer" ] || answer=$HOME/Projects
        # shellcheck disable=SC2088 # the ~ the person typed
        case $answer in
            '~') path=$HOME ;;
            '~/'*) path=$HOME/${answer#'~/'} ;;
            '~'*) path='' ;;  # another user's home: tmux-workspace setup knows it
            /*) path=$answer ;;
            *) path=$HOME/$answer ;;
        esac
        if [ -n "$path" ] && [ -e "$path" ] && [ ! -d "$path" ]; then
            printf '%s is a file, not a folder. Choose another folder.\n' "$(ui_path "$path")"
            continue
        fi
        PROJECTS=$answer
        return 0
    done
}

chosen_packages() {  # the packages of the ticked rows
    local i list=''
    for ((i = 0; i < NROWS; i++)); do
        if [ -n "${R_PKG[i]}" ] && [ "${R_ON[i]}" = 1 ]; then list="${list:+$list }${R_PKG[i]}"; fi
    done
    printf '%s\n' "$list"
}

sudo_ready() {  # sudo asks for the password now, before a spinner covers its prompt
    local packages
    packages=$(chosen_packages)
    [ -n "$SUDO" ] && [ -n "$packages" ] || return 0
    printf '\nInstalling %s takes sudo.\n' "$packages"
    sudo -v || die "sudo did not run; nothing was changed" 1
}

install_chosen() {  # a step for each ticked package row; tmux is checked again once it is installed
    local i first=1 what
    for ((i = 0; i < NROWS; i++)); do
        [ -n "${R_PKG[i]}" ] && [ "${R_ON[i]}" = 1 ] || continue
        step "${R_NAME[i]}" "$(install_command "${R_PKG[i]}")" slow
        what="leave ${R_NAME[i]} unticked"
        [ "${R_KIND[i]}" != need ] || what="install it yourself"
        STEP_FIX="$PM could not install ${R_PKG[i]}. Fix what the log says (or $what), then run the installer again: $(ui_rerun)"
        if [ "$first" = 1 ] && [ "$PM" = apt-get ]; then $SUDO apt-get update -qq </dev/null; fi
        first=0
        pm_install "${R_PKG[i]}" </dev/null
        if [ "${R_PKG[i]}" = tmux ]; then check_requirements_again; fi
    done
}

guided_setup() {  # the checklist, the projects folder, and the tools it asked for
    local i
    survey
    checklist
    ask_projects
    TMUXCONF_AGREED=1  # the tmux config row said a file of the user's moves to a backup
    if [ "$NAGENTS" -gt 0 ]; then
        AGENTS=0
        for ((i = 0; i < NROWS; i++)); do
            if [ "${R_AGENT[i]}" = 1 ] && [ "${R_ON[i]}" = 1 ]; then AGENTS=1; fi
        done
    fi
    sudo_ready
    install_chosen
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

# A file the user already has (not one of ours) is replaced only with --yes or a yes on the terminal
# (AGREED 1: the checklist said so). The lines per step never stop to ask: they keep the file.
link_asking() {  # link_asking SOURCE DEST [AGREED]
    local source=$1 dest=$2 agreed=${3:-$YES}
    ours "$dest" "$source" && return 0
    if { [ -e "$dest" ] || [ -L "$dest" ]; } && [ "$agreed" = 0 ]; then
        if [ "$DRY" = 1 ] || [ "$UI" != plain ] || ! ask "Replace your $dest? It moves to a backup and --uninstall restores it."; then
            say skip "$dest exists; rerun with --yes to replace it (a backup is kept)"
            KEPT="$KEPT$dest"$'\n'
            STEP_NOTE='kept yours'
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
        step 'projects folder' "$(ui_path "$PROJECTS")"
        "$tool" setup --projects "$PROJECTS" >/dev/null
        say setup "projects folder $PROJECTS"
        changed
    elif grep -q '"projects_dir"' "$CONFIG_DIR/config.json" 2>/dev/null; then
        return 0
    elif [ "$YES" = 0 ] && [ "$DRY" = 0 ] && [ "$UI" = plain ] && tty_ok; then
        "$tool" setup </dev/tty >/dev/tty || say later "projects folder: the first \`work\` asks"
        changed
    else
        step 'projects folder' "the first \`work\` asks" quick
        say later "projects folder: the first \`work\` asks (or rerun with --projects DIR)"
    fi
}

do_install() {
    local codex_hooks
    say repo "$REPO"
    step tmux-workspace "$(ui_path "$BIN_DIR/tmux-workspace")"
    link "$REPO/bin/tmux-workspace" "$BIN_DIR/tmux-workspace"
    link "$REPO/bin/tw-clip" "$BIN_DIR/tw-clip"
    link "$REPO/bin/tw-sound" "$BIN_DIR/tw-sound"
    link "$REPO/agent-status.conf" "$STATUS_CONF"
    # An update keeps what the install chose: the links and the shell block lead through REPO, which
    # now holds the new version, and what the install left out (--no-shell, --no-tmux-conf, a file
    # the user kept) stays out.
    if [ "$UPDATE" = 0 ]; then
        if [ "$TMUXCONF" = 1 ]; then
            step 'tmux config' "$(ui_path "$HOME/.tmux.conf")"
            link_asking "$REPO/tmux.conf" "$HOME/.tmux.conf" "$TMUXCONF_AGREED"
        fi
        if has sesh; then
            step 'sesh config' "$(ui_path "$HOME_CONFIG/sesh/sesh.toml")"
            link_asking "$REPO/sesh.toml" "$HOME_CONFIG/sesh/sesh.toml"
        fi
        if [ "$SHELLS" = 1 ]; then
            if [ "$UI" != plain ]; then step 'work command' "$(shell_targets)"; fi
            shell_rc "${ZDOTDIR:-$HOME}/.zshrc" zsh
            shell_rc "$HOME/.bashrc" bash
            if has fish || [ -d "$HOME_CONFIG/fish" ]; then
                link_asking "$REPO/shell/work.fish" "$HOME_CONFIG/fish/functions/work.fish"
            fi
        fi
    fi
    step config "$(ui_path "$CONFIG_DIR/config.json")"
    migrate_config
    if [ "$AGENTS" = 1 ] || { [ "$UPDATE" = 1 ] && integrations_installed; }; then
        if [ "$UI" != plain ]; then step 'agent status' "$(agent_names)"; fi
        codex_hooks=$(cat "$(codex_home)/hooks.json" 2>/dev/null || true)
        act agents "Claude Code, Codex, omp and Neovim where installed" \
            env TMUX_WORKSPACE_SHARE="$REPO" "$REPO/bin/tmux-workspace" integrations install
        if [ "$codex_hooks" != "$(cat "$(codex_home)/hooks.json" 2>/dev/null || true)" ]; then CODEX_CHANGED=1; fi
    fi
    if [ "$DRY" = 1 ]; then
        [ "$UPDATE" = 1 ] || say would "ask for your projects folder unless it is set"
        say "done" "dry run: nothing changed"
        return 0
    fi
    if [ "$UPDATE" = 1 ]; then
        finish_update
        return 0
    fi
    onboard
    if [ "$CHANGED" = 0 ]; then
        say "done" "tmux-workspace already installed"
        summary_install
        return 0
    fi
    if [ "$TMUXCONF" = 1 ] && has tmux && tmux info >/dev/null 2>&1; then
        step 'tmux reloaded' 'the running server' quick
        tmux source-file "$HOME/.tmux.conf" && say reload "the running tmux server"
    fi
    "$BIN_DIR/tmux-workspace" doctor >/dev/null 2>&1 || true
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) say note "$BIN_DIR is not on PATH yet; new shells get it from the work command block" ;;
    esac
    say "done" "tmux-workspace installed"
    # UPDATE_FROM names the version this upgrades from (a v1 clone, pulled in place), so terminals are
    # open already. A first install has none, and `next` tells it to open one.
    if [ -n "$UPDATE_FROM" ]; then say_reload_shells; fi
    say next "open a new terminal, then: work my-project   (Ctrl+a ? lists every shortcut)"
    summary_install
}

# The end of a guided or styled install: what changed, how to undo it, and what to do next.
summary_install() {
    [ "$UI" != plain ] || return 0
    local version path advice
    step_end
    version=$(ui_version)
    ui ''
    if [ "$CHANGED" = 0 ]; then
        ui "${C_BOLD}tmux-workspace $version is installed already.$C_OFF Nothing changed."
    else
        ui "${C_BOLD}tmux-workspace $version is installed.$C_OFF"
        if [ -n "$DONE_NAMES" ]; then ui "  Changed    $DONE_NAMES"; fi
        { printf '%s\n' "$BACKUP_DIR" && log_since | sed -n 's/^backups in //p'; } | while IFS= read -r path; do
            if [ -n "$path" ]; then ui "  Backups    $(ui_path "$path")"; fi
        done
        ui "  Undo       $(ui_path "$REPO")/install.sh --uninstall"
    fi
    printf '%s' "$KEPT" | while IFS= read -r path; do
        ui "  Kept       your $(ui_path "$path"); rerun with --yes to replace it (a backup is kept)"
    done
    version=$(python_version)
    if [ -z "$version" ] || ! version_ge "$version" "$PYTHON_MIN"; then
        ui "  Note       dash, ai and cleanup need python3 $PYTHON_MIN+"
    fi
    ui ''
    ui "${C_BOLD}Next$C_OFF"
    ui "  1. Open a new terminal, or run: exec \$SHELL"
    ui '  2. work my-project'
    ui '  3. Ctrl+a ? lists every shortcut'
    if [ "$CODEX_CHANGED" = 1 ]; then
        ui '  Codex: review the new hooks with /hooks in Codex, then restart running Codex CLIs.'
    fi
    log_since | sed -n '/^Neovim: /p; /^iTerm2: /p' | while IFS= read -r advice; do ui "  $advice"; done
}

log_since() { awk -v start="$LOG_START" 'NR > start' "$LOG"; }  # what this run wrote to LOG

# A terminal that is open keeps the work command it started with, which may be an older one.
say_reload_shells() {
    say reload "run exec \$SHELL in terminals that are already open, so they get the new work command"
}

# Whether the agent integrations are installed: an update registers them again only then.
integrations_installed() {
    [ "$(TMUX_WORKSPACE_SHARE="$REPO" "$REPO/bin/tmux-workspace" integrations status 2>/dev/null || true)" = installed ]
}

installed_label() {  # the version REPO holds, as the update messages name it
    if [ "$KIND" = release ]; then
        release_version "$REPO"
    elif [ -e "$REPO/.git" ] && has git; then
        clone_label "$REPO" HEAD
    else
        tree_version "$REPO"
    fi
}

finish_update() {
    if ours "$HOME/.tmux.conf" "$REPO/tmux.conf" && has tmux && tmux info >/dev/null 2>&1; then
        step 'tmux reloaded' 'the running server' quick
        tmux source-file "$HOME/.tmux.conf" && say reload "the running tmux server"
    fi
    "$BIN_DIR/tmux-workspace" doctor >/dev/null 2>&1 || true
    say "done" "updated ${UPDATE_FROM:-an earlier version} → $(installed_label)"
    say_reload_shells
    [ "$UI" != plain ] || return 0
    ui_done "Updated ${UPDATE_FROM:-an earlier version} to $(installed_label)."
    ui "  Run exec \$SHELL in terminals that are already open, so they get the new work command."
    if [ "$CODEX_CHANGED" = 1 ]; then
        ui '  Codex: review the new hooks with /hooks in Codex, then restart running Codex CLIs.'
    fi
}

ui_done() {  # ui_done TEXT: the last step is done, and TEXT says how it ended
    [ "$UI" != plain ] || return 0
    step_end
    ui ''
    ui "$C_BOLD$1$C_OFF"
}

confirm_uninstall() {  # one question before an uninstall on a terminal
    printf '%s\n' "${C_BOLD}tmux-workspace $(ui_version)$C_OFF"
    if ! ask "${C_BOLD}Remove tmux-workspace?$C_OFF Its links, the work command and the agent hooks go, and what it replaced comes back."; then
        printf '%s\n' 'Nothing was changed.'
        exit 0
    fi
    SAME_NOTE='nothing to remove'
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
        step 'agent integrations' 'what --agents registered' quick
        TMUX_WORKSPACE_SHARE="$REPO" "$REPO/bin/tmux-workspace" integrations uninstall | while IFS= read -r line; do
            if [ -n "$line" ]; then say remove "$line"; fi
        done
    fi
    step links "$(ui_path "$BIN_DIR/tmux-workspace"), $(ui_path "$HOME/.tmux.conf") and the rest"
    for dest in "$BIN_DIR/tmux-workspace" "$BIN_DIR/tw-clip" "$BIN_DIR/tw-sound" "$STATUS_CONF" \
        "$HOME/.tmux.conf" "$HOME_CONFIG/sesh/sesh.toml" "$HOME_CONFIG/fish/functions/work.fish"; do
        if [ -L "$dest" ] && owned "$dest"; then act remove "$dest" rm -f "${dest:?}"; fi
    done
    if [ "$UI" != plain ]; then step 'work command' "$(rc_files)"; fi
    for rc in "${ZDOTDIR:-$HOME}/.zshrc" "$HOME/.bashrc"; do
        [ -e "$rc" ] || continue
        target=$(resolve "$rc")
        if grep -qF "$BLOCK_START" "$target"; then act edit "$rc (removes the work command)" strip_block "$target"; fi
    done
    # Newest backups first; a path that is back already keeps its newer content.
    step backups 'put back'
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
        step 'tmux server' 'no longer starts panes through tmux-workspace'
        act tmux "the running server no longer starts panes through tmux-workspace" stop_server_hooks
        say note "restart tmux to finish: tmux kill-server, then start it again"
    fi
    if [ "$CHANGED" = 0 ]; then say "done" "nothing to remove"; else say "done" "tmux-workspace removed; snapshots and $CONFIG_DIR stay"; fi
    if [ "$KIND" = release ] && [ "$CHANGED" = 1 ]; then
        say note "the program files stay in $HOME_SHARE; delete that folder to remove them"
    fi
    [ "$UI" != plain ] || return 0
    if [ "$CHANGED" = 0 ]; then
        ui_done 'Nothing to remove.'
        return 0
    fi
    ui_done "tmux-workspace is removed.$C_OFF Snapshots and $(ui_path "$CONFIG_DIR") stay."
    if [ "$KIND" = release ]; then
        ui "  The program files stay in $(ui_path "$HOME_SHARE"); delete that folder to remove them."
    fi
    if [ "$ours_conf" = 1 ] && has tmux && tmux info >/dev/null 2>&1; then
        ui '  Restart tmux to finish: tmux kill-server, then start it again.'
    fi
}

rc_files() {  # the shell startup files there are, which hold the work command
    local list='' rc
    for rc in "${ZDOTDIR:-$HOME}/.zshrc" "$HOME/.bashrc"; do
        if [ -e "$rc" ]; then list="${list:+$list, }$(ui_path "$rc")"; fi
    done
    printf '%s\n' "${list:-none}"
}

main() {
    choose_ui
    REPO=$(script_dir || true)
    if [ -z "$REPO" ] || ! is_tree "$REPO"; then
        remote_install
    fi
    detect_platform
    if [ "$UI" = styled ]; then
        if [ "$UNINSTALL" = 1 ]; then confirm_uninstall; fi
        progress_open
    fi
    if [ -f "$REPO/Cargo.toml" ]; then
        KIND=clone
        OWN=$REPO
        if [ -n "$WANTED" ]; then say note "--version is for release installs; in a clone, check out $WANTED instead"; fi
        if [ "$UPDATE" = 1 ] && [ -z "$UPDATE_FROM" ]; then update_clone; fi
    else
        KIND=release
        if [ "$UPDATE" = 1 ] && [ -z "$UPDATE_FROM" ] && ! staged_here; then update_release; fi
        settle_release
        OWN=$(cd "$HOME_SHARE" 2>/dev/null && pwd -P || printf '%s' "$HOME_SHARE")
    fi
    if [ "$UNINSTALL" = 1 ]; then
        do_uninstall
        return 0
    fi
    check_requirements
    if [ "$KIND" = clone ]; then install_binary; fi
    # Handed over after a pull or a download: what the links lead to changed, even when they did not.
    if [ -n "$UPDATE_FROM" ]; then CHANGED=1; fi
    do_install
}

main
