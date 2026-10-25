"""Read-only warm picker latency: python3 tests/benchmark_workspace.py [source]."""
import json
from pathlib import Path
import runpy
import statistics
import subprocess
import sys
import time
from unittest.mock import patch


def benchmark(source):
    workspace = runpy.run_path(str(source))
    if workspace['option']('@workspace_initialized') != '1':
        raise SystemExit('Start the workspace first; benchmark never restores or creates sessions.')
    original = subprocess.run
    for name in ('choose_or_create_workspace', 'pick_window'):
        durations, counts = [], []
        for _ in range(15):
            calls = []

            def run(args, **kwargs):
                if args[0] == 'fzf':
                    return subprocess.CompletedProcess(args, 130, '')
                calls.append(args[0])
                return original(args, **kwargs)

            start = time.perf_counter()
            with patch.object(subprocess, 'run', side_effect=run), \
                    patch.object(sys.stdin, 'isatty', return_value=True), \
                    patch.object(sys.stdout, 'isatty', return_value=True):
                workspace[name]()
            durations.append((time.perf_counter() - start) * 1000)
            counts.append(len(calls))
        print(json.dumps(dict(picker=name, p50_ms=round(statistics.median(durations), 1),
                              p95_ms=round(sorted(durations)[-1], 1), samples=len(durations),
                              subprocesses=max(counts))))


if __name__ == '__main__':
    benchmark(Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[1] / 'bin/tmux-workspace')
