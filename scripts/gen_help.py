#!/usr/bin/env python3
"""Generate src/help_topics.rs from the Python controller's HELP_TOPICS.

  gen_help.py                write src/help_topics.rs
  gen_help.py --print ID...  print what the Python `help-topic ID` prints for each (the Rust tests compare with it)
  gen_help.py --list         print the lines the Python `help` gives fzf: ID, a tab, the display text

The controller is legacy/bin/tmux-workspace once it has moved there, else bin/tmux-workspace.
"""
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / 'src' / 'help_topics.rs'
FIELDS = ('id', 'display', 'title', 'text')
HEADER = """\
//! The help topics, generated from the Python controller's HELP_TOPICS by scripts/gen_help.py.
//! Do not edit by hand: change the topics there and run the script again.

use crate::help::Topic;

pub const TOPICS: &[Topic] = &[
"""


def controller():
    for path in (ROOT / 'legacy' / 'bin' / 'tmux-workspace', ROOT / 'bin' / 'tmux-workspace'):
        if path.is_file():
            return path
    raise SystemExit('gen_help.py: no legacy/bin/tmux-workspace or bin/tmux-workspace')


def load_topics():
    """Load the controller as a module the way tests/support.py does. It has no .py suffix and sets the umask."""
    path = str(controller())
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_loader('tmux_workspace', importlib.machinery.SourceFileLoader('tmux_workspace', path))
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    mask = os.umask(0)
    try:
        spec.loader.exec_module(module)
    finally:
        os.umask(mask)
    return module


def raw(text):
    """A Rust raw string literal with as many # as the text needs."""
    for bad in ('\r', '\0'):
        assert bad not in text, f'{text[:40]!r} holds {bad!r}, which a Rust source file cannot keep as it is'
    hashes = 2
    while '"' + '#' * hashes in text:
        hashes += 1
    mark = '#' * hashes
    return f'r{mark}"{text}"{mark}'


def rust(topics):
    out = [HEADER]
    for topic in topics:
        out.append('    Topic {\n')
        out.extend(f'        {name}: {raw(topic[name])},\n' for name in FIELDS)
        out.append('    },\n')
    out.append('];\n')
    return ''.join(out)


def main(args):
    module = load_topics()
    topics = module.HELP_TOPICS
    if args == ['--list']:
        print('\n'.join(f"{t['id']}\t{t['display']}" for t in topics))
    elif len(args) > 1 and args[0] == '--print':
        for topic_id in args[1:]:
            module.help_topic(topic_id)
    elif not args:
        OUTPUT.write_text(rust(topics), encoding='utf-8')
        print(f'{OUTPUT.relative_to(ROOT)}: {len(topics)} topics')
    else:
        raise SystemExit(__doc__)


if __name__ == '__main__':
    sys.stdout.reconfigure(encoding='utf-8')  # The same bytes whatever the locale.
    main(sys.argv[1:])
