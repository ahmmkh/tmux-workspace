#!/usr/bin/env python3
"""Publish only the originating interactive agent's exact session to its tmux pane."""
import json
import os
import re
import shlex
import shutil
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

TMUX = "/opt/homebrew/bin/tmux"
PS = "/bin/ps"
OPTION = "@workspace_app"
STATUS = "@workspace_agent"
WAIT = "@workspace_agent_wait"
FAILED = "@workspace_agent_failed"
SESSIONS = os.path.expanduser("~/.claude/sessions")
SETTLE = 5000  # Milliseconds within which a failed turn writes its own idle state.
# One read gives the pane's process, its status options, the turn-over value and its identity.
PANE = "\t".join(("#{pane_pid}", "#{pane_tty}", "#{" + STATUS + "}", "#{" + WAIT + "}",
                  "#{" + FAILED + "}", "#{E:@agent_turn_over}", "#{" + OPTION + "}"))


def run(argv):
    return subprocess.run(argv, check=True, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, timeout=2).stdout.strip()


def process_kind(pid, command):
    """Use the executable, not occurrences of agent names in shell/tool arguments."""
    name = os.path.basename(command)
    if name in ("claude", "codex", "omp", "nvim"):
        return name
    args = shlex.split(run([PS, "-ww", "-p", str(pid), "-o", "args="]))
    if not args:
        return None
    executable = os.path.realpath(shutil.which(args[0]) or args[0])
    if "/.local/share/claude/versions/" in executable:
        return "claude"
    if name in ("bun", "node") and len(args) > 1:
        script = os.path.realpath(args[1])
        if script.endswith("/@oh-my-pi/pi-coding-agent/dist/cli.js"):
            return "omp"
        if script.endswith("/@anthropic-ai/claude-code/cli.js"):
            return "claude"
        if script.endswith("/@openai/codex/bin/codex.js"):
            return "codex-wrapper"
    return None


def process_table():
    processes = {}
    for line in run([PS, "-axo", "pid=,ppid=,tty=,comm="]).splitlines():
        fields = line.split(None, 3)
        if len(fields) == 4:
            processes[int(fields[0])] = (int(fields[1]), fields[2], fields[3])
    return processes


def owner(kind, pane_pid, pane_tty, explicit_pid=None, start_pid=None, processes=None):
    if processes is None:
        processes = process_table()
    pid = os.getppid() if start_pid is None else start_pid
    candidates = []
    visited = set()
    while pid in processes and pid not in visited:
        visited.add(pid)
        parent, tty, command = processes[pid]
        app = process_kind(pid, command)
        if app:
            args = shlex.split(run([PS, "-ww", "-p", str(pid), "-o", "args="]))
            # A local Codex app-server is a child of the actual interactive CLI.
            # Standalone app-server, exec, print, and inherited tool jobs are not panes.
            if app == "codex" and len(args) > 1 and args[1] == "app-server":
                pass
            elif app != "codex-wrapper":
                candidates.append((pid, app, tty, args))
        if pid == pane_pid:
            break
        pid = parent
    else:
        return None
    # Reject nested agents/editors, even when a tool inherited TMUX and TMUX_PANE.
    if len(candidates) != 1:
        return None
    pid, app, tty, args = candidates[0]
    if app != kind or (explicit_pid is not None and pid != explicit_pid):
        return None
    if tty != pane_tty.removeprefix("/dev/"):
        return None
    groups = run([PS, "-p", str(pid), "-o", "pgid=,tpgid="]).split()
    if len(groups) != 2 or groups[0] != groups[1] or int(groups[0]) <= 0:
        return None
    if kind == "claude" and any(arg in ("-p", "--print") for arg in args[1:]):
        return None
    if kind == "omp" and any(arg in ("-p", "--print", "--mode", "--no-session") for arg in args[1:]):
        return None
    if kind == "codex" and len(args) > 1 and args[1] in (
            "exec", "e", "review", "app-server", "mcp-server", "debug"):
        return None
    birth = run([PS, "-p", str(pid), "-o", "lstart="])
    return (pid, birth) if birth else None


def resolve_codex(pane_pid, pane_tty, pane_title, pane_cwd=None):
    """Resolve the foreground CLI's current title, never a latest/unique-old rollout."""
    processes = process_table()
    identities = set()
    for pid, (_parent, tty, command) in processes.items():
        if tty != pane_tty.removeprefix("/dev/") or process_kind(pid, command) != "codex":
            continue
        identity = owner("codex", pane_pid, pane_tty, pid, pid, processes)
        if identity:
            identities.add(identity)
    if not identities:
        return None
    if len(identities) != 1:
        return {"kind": "codex", "error": "Multiple foreground Codex owners"}
    pid, birth = identities.pop()
    error = {"kind": "codex", "pid": pid, "birth": birth}
    # 0.153.4 truncates its native thread-id title item to 29 chars plus "...".
    # It still identifies the CURRENT ChatWidget immediately, before any prompt.
    full_uuid = r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
    truncated_uuid = r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{5}\.\.\."
    match = re.fullmatch(r"codex \| (" + full_uuid + "|" + truncated_uuid + ")", pane_title)
    if not match:
        return {**error, "error": "Codex has not published a current thread-id title"}
    visible_id = match.group(1)
    prefix = visible_id[:-3] if visible_id.endswith("...") else visible_id
    files = []
    descriptor = access = None
    for line in run(["/usr/sbin/lsof", "-a", "-p", str(pid), "-Ffan"]).splitlines():
        if line.startswith("f"):
            descriptor, access = line[1:], None
        elif line.startswith("a"):
            access = line[1:]
        elif line.startswith("n") and os.path.isabs(line[1:]):
            files.append((descriptor, access, line[1:]))
    matches = {}
    cwd = pane_cwd if isinstance(pane_cwd, str) and os.path.isabs(pane_cwd) else None
    for descriptor, access, path in files:
        if descriptor == "cwd" and cwd is None:
            cwd = path
        if access not in ("w", "u") or not re.fullmatch(r"rollout-.*\.jsonl", os.path.basename(path)):
            continue
        if prefix not in os.path.basename(path):
            continue
        with open(path, encoding="utf-8") as rollout:
            first = rollout.readline(1024 * 1024 + 1)
        if len(first) > 1024 * 1024:
            continue
        header = json.loads(first)
        payload = header.get("payload", {})
        session_id = payload.get("id", "")
        if header.get("type") == "session_meta" and re.fullmatch(full_uuid, session_id) and session_id.startswith(prefix):
            if payload.get("parent_thread_id") or isinstance(payload.get("source"), dict):
                return {**error, "error": "Codex is displaying a subagent, not a root conversation"}
            matches[session_id] = payload.get("cwd", cwd)
    # Read only databases actually open in this PID. The title prefix is the key;
    # database recency, global latest, and unrelated rollout writers are irrelevant.
    databases = {path for _fd, _access, path in files
                 if re.fullmatch(r"state_\d+\.sqlite", os.path.basename(path))}
    for path in databases:
        with sqlite3.connect(Path(path).as_uri() + "?mode=ro", uri=True, timeout=1) as db:
            rows = db.execute("SELECT id, cwd, source FROM threads WHERE id LIKE ? LIMIT 2",
                              (prefix + "%",)).fetchall()
        for session_id, recorded_cwd, source in rows:
            if not re.fullmatch(full_uuid, session_id):
                continue
            if source not in ("cli", "vscode", "appServer", "unknown"):
                return {**error, "error": "Current Codex title resolves to a non-root thread"}
            matches[session_id] = recorded_cwd
    if len(matches) != 1:
        return {**error, "error": "Current Codex thread is not persisted or its title prefix is ambiguous"}
    session_id, recorded_cwd = next(iter(matches.items()))
    cwd = recorded_cwd if isinstance(recorded_cwd, str) and os.path.isabs(recorded_cwd) else cwd
    if cwd is None:
        return {**error, "error": "Cannot establish current Codex working directory"}
    if run([PS, "-p", str(pid), "-o", "lstart="]) != birth:
        return {**error, "error": "Codex process changed during identity lookup"}
    return {"kind": "codex", "id": session_id, "pid": pid, "cwd": cwd, "birth": birth}


def clear_owned(tmux, pane, metadata):
    raw = run([*tmux, "show-option", "-p", "-v", "-q", "-t", pane, OPTION])
    if not raw:
        return
    current = json.loads(raw)
    if any(current.get(key) != metadata.get(key) for key in ("kind", "id", "pid", "birth")):
        return
    # Compare-and-clear inside the server, not a racy read followed by an unset.
    # The status goes with the identity, because both belong to the same agent.
    set_if(tmux, pane, OPTION, raw, " ; ".join(
        "set-option -pu -t " + pane + " " + option for option in (OPTION, STATUS, WAIT, FAILED)))


def pane_state(tmux, pane):
    fields = run([*tmux, "display-message", "-p", "-t", pane, PANE]).split("\t", 6)
    return fields + [""] * (7 - len(fields))


def assign(pane, option, value):
    return "set-option -p -t " + pane + " " + option + " '" + value.replace("'", "'\\''") + "'"


def same(option, old):
    escaped = old.replace("#", "##").replace(",", "#,").replace("}", "#}")
    return "#{==:#{" + option + "}," + escaped + "}"


def set_if(tmux, pane, option, old, command, also=None):
    """Run the command only while the option still holds the value that was read."""
    guard = same(option, old)
    if also:
        guard = "#{&&:" + guard + "," + same(*also) + "}"
    run([*tmux, "if-shell", "-F", "-t", pane, guard, command])


def session_state(pid):
    """Read the live state that Claude Code writes for each interactive session."""
    try:
        with open(os.path.join(SESSIONS, str(pid) + ".json"), encoding="utf-8") as state:
            return json.load(state)
    except (OSError, ValueError):
        return None


def wanted_status(state, session_id, wait, failed, turn_over):
    """Map the state file to a pane status. None means the pane keeps what it has."""
    if not isinstance(state, dict):
        return None
    status = state.get("status")
    updated = state.get("statusUpdatedAt")

    def newer(margin):
        return not wait.isdigit() or (isinstance(updated, (int, float)) and updated > int(wait) + margin)

    if status == "waiting":
        return "blocked"
    if status == "busy":
        # A hook that set blocked wins until the file changes after that hook started.
        return "working" if newer(0) else None
    if status in ("idle", "shell"):
        # A failed turn stays blocked. The once-a-second call can miss a later turn shorter
        # than a second, so an idle file that changed well after the failure also ends it.
        # ponytail: a later turn that is missed and ends within SETTLE of the failure keeps
        # blocked until the next prompt. A UserPromptSubmit hook would close that.
        if failed == session_id and not newer(SETTLE):
            return None
        return turn_over or None
    return None


def main():
    started = int(time.time() * 1000)
    if len(sys.argv) != 2 or sys.argv[1] not in ("claude", "claude-status", "omp"):
        return
    pane = os.environ.get("TMUX_PANE", "")
    socket = os.environ.get("TMUX", "").rsplit(",", 2)
    if len(socket) != 3 or not os.path.isabs(socket[0]) or not re.fullmatch(r"%\d+", pane):
        return
    status_line = sys.argv[1] == "claude-status"
    kind = "claude" if status_line else sys.argv[1]
    data = json.load(sys.stdin)
    if not isinstance(data, dict):
        return
    event = "SessionStart" if status_line else data.get("hook_event_name")
    # A subagent's approval reaches the user through the pane's agent, so it counts for blocked.
    blocking = kind == "claude" and event in ("PermissionRequest", "PreToolUse", "StopFailure")
    if not blocking and (data.get("agent_id") or event not in ("SessionStart", "SessionEnd", "CwdChanged")):
        return
    session_id = data.get("session_id")
    cwd = data.get("cwd")
    if not isinstance(session_id, str) or not session_id or len(session_id) > 4096:
        return
    if any(ord(char) < 32 for char in session_id):
        return
    tmux = [TMUX, "-N", "-S", socket[0]]
    if blocking:
        if event == "PreToolUse" and data.get("tool_name") not in ("AskUserQuestion", "ExitPlanMode"):
            return
        app = pane_state(tmux, pane)[6]
        current = json.loads(app) if app else {}
        # Only the pane's own Claude session may block it, never a nested claude -p.
        if current.get("kind") != "claude" or current.get("id") != session_id:
            return
        failed = [";", "set-option", "-p", "-t", pane, FAILED, session_id] if event == "StopFailure" else []
        run([*tmux, "set-option", "-p", "-t", pane, STATUS, "blocked", ";",
             "set-option", "-p", "-t", pane, WAIT, str(started), *failed])
        return
    if not isinstance(cwd, str) or not os.path.isabs(cwd):
        return
    if any(ord(char) < 32 for char in cwd):
        return
    explicit_pid = data.get("pid") if kind == "omp" else None
    if kind == "omp" and (type(explicit_pid) is not int or explicit_pid <= 1):
        return
    state = pane_state(tmux, pane)
    identity = owner(kind, int(state[0]), state[1], explicit_pid)
    if identity is None:
        return
    pid, birth = identity
    metadata = {"kind": kind, "id": session_id, "pid": pid, "cwd": cwd, "birth": birth}
    if kind == "claude" and not status_line and event == "SessionStart" and data.get("source") == "fork":
        # /fork may copy a BACKGROUND conversation under the same PID. Invalidate
        # the old owner until the native status callback identifies the visible one.
        raw = run([*tmux, "show-option", "-p", "-v", "-q", "-t", pane, OPTION])
        current = json.loads(raw) if raw else {}
        if all(current.get(key) == metadata[key] for key in ("kind", "pid", "birth")):
            clear_owned(tmux, pane, current)
        return
    if event == "SessionEnd":
        clear_owned(tmux, pane, metadata)
    else:
        value = json.dumps(metadata, separators=(",", ":"))
        if value != state[6]:
            set_if(tmux, pane, OPTION, state[6], assign(pane, OPTION, value))
    if status_line:
        print("Claude | " + session_id[:8])
        wanted = wanted_status(session_state(pid), session_id, state[3], state[4], state[5])
        if wanted and wanted != state[2]:
            # Any status the file gives ends a failed turn, so the mark goes with the write.
            # A hook that blocks an already blocked pane changes only the wait value.
            set_if(tmux, pane, STATUS, state[2],
                   assign(pane, STATUS, wanted) + " ; set-option -pu -t " + pane + " " + FAILED,
                   (WAIT, state[3]))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        # Recovery metadata must never change hook approval decisions or agent output.
        pass
