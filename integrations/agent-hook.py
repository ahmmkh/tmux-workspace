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
from pathlib import Path

TMUX = "/opt/homebrew/bin/tmux"
PS = "/bin/ps"
OPTION = "@workspace_app"


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
    escaped = raw.replace("#", "##").replace(",", "#,").replace("}", "#}")
    run([*tmux, "if-shell", "-F", "-t", pane,
         "#{==:#{" + OPTION + "}," + escaped + "}",
         "set-option -pu -t " + pane + " " + OPTION])


def main():
    if len(sys.argv) != 2 or sys.argv[1] not in ("claude", "claude-status", "omp"):
        return
    pane = os.environ.get("TMUX_PANE", "")
    socket = os.environ.get("TMUX", "").rsplit(",", 2)
    if len(socket) != 3 or not os.path.isabs(socket[0]) or not re.fullmatch(r"%\d+", pane):
        return
    status_line = sys.argv[1] == "claude-status"
    kind = "claude" if status_line else sys.argv[1]
    data = json.load(sys.stdin)
    if not isinstance(data, dict) or data.get("agent_id"):
        return
    event = "SessionStart" if status_line else data.get("hook_event_name")
    if event not in ("SessionStart", "SessionEnd", "CwdChanged"):
        return
    session_id = data.get("session_id")
    cwd = data.get("cwd")
    if not isinstance(session_id, str) or not session_id or len(session_id) > 4096:
        return
    if not isinstance(cwd, str) or not os.path.isabs(cwd):
        return
    if any(ord(char) < 32 for char in session_id + cwd):
        return
    explicit_pid = data.get("pid") if kind == "omp" else None
    if kind == "omp" and (type(explicit_pid) is not int or explicit_pid <= 1):
        return
    tmux = [TMUX, "-N", "-S", socket[0]]
    pane_info = run([*tmux, "display-message", "-p", "-t", pane,
                     "#{pane_pid}\t#{pane_tty}"]).split("\t")
    if len(pane_info) != 2:
        return
    identity = owner(kind, int(pane_info[0]), pane_info[1], explicit_pid)
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
        run([*tmux, "set-option", "-p", "-t", pane, OPTION,
             json.dumps(metadata, separators=(",", ":"))])
    if status_line:
        print("Claude | " + session_id[:8])


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        # Recovery metadata must never change hook approval decisions or agent output.
        pass
