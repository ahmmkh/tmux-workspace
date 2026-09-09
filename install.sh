#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# TMUX WORKSPACE AUTOMATED INSTALLER
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BACKUP_DIR="$HOME/.local/state/tmux-workspace/backups/install-$(date +%Y%m%d-%H%M%S)"

echo "==> Setting up tmux-workspace..."

# 1. Check prerequisites
echo "--> Checking dependencies..."
MISSING_DEPS=()
for bin in tmux fzf zoxide sesh python3; do
    if ! command -v "$bin" >/dev/null 2>&1; then
        MISSING_DEPS+=("$bin")
    fi
done

if [ ${#MISSING_DEPS[@]} -gt 0 ]; then
    echo "    Missing tools: ${MISSING_DEPS[*]}"
    if command -v brew >/dev/null 2>&1; then
        echo "    Installing missing dependencies via Homebrew..."
        brew install "${MISSING_DEPS[@]}"
    else
        echo "    [WARN] Please install: ${MISSING_DEPS[*]}"
    fi
else
    echo "    All dependencies installed (tmux, fzf, zoxide, sesh, python3)."
fi

# 2. Create destination directories
echo "--> Creating required directories..."
mkdir -p "$HOME/.local/bin" \
         "$HOME/.local/lib/tmux-workspace" \
         "$HOME/.config/tmux" \
         "$HOME/.config/sesh" \
         "$HOME/.config/nvim/lua/custom" \
         "$HOME/.omp/agent/extensions" \
         "$HOME/.local/state/tmux-workspace" \
         "$HOME/untitled folder/Projects" \
         "$BACKUP_DIR"

backup_if_exists() {
    local target="$1"
    if [ -e "$target" ] && [ ! -L "$target" ]; then
        cp -a "$target" "$BACKUP_DIR/"
        echo "    Backed up $(basename "$target") -> $BACKUP_DIR/"
    fi
}

# 3. Install core files
echo "--> Installing configuration files..."

backup_if_exists "$HOME/.tmux.conf"
cp "$SCRIPT_DIR/tmux/tmux.conf" "$HOME/.tmux.conf"

cp "$SCRIPT_DIR/bin/tmux-workspace" "$HOME/.local/bin/tmux-workspace"
chmod +x "$HOME/.local/bin/tmux-workspace"

cp "$SCRIPT_DIR/lib/agent-hook.py" "$HOME/.local/lib/tmux-workspace/agent-hook.py"
chmod +x "$HOME/.local/lib/tmux-workspace/agent-hook.py"

if [ ! -f "$HOME/.config/sesh/sesh.toml" ]; then
    cp "$SCRIPT_DIR/sesh/sesh.toml" "$HOME/.config/sesh/sesh.toml"
else
    # Update wildcard if missing
    if ! grep -q "untitled folder/Projects" "$HOME/.config/sesh/sesh.toml" 2>/dev/null; then
        echo -e '\n[[wildcard]]\npattern = "~/untitled folder/Projects/*"' >> "$HOME/.config/sesh/sesh.toml"
    fi
fi

cp "$SCRIPT_DIR/WORKSPACE_GUIDE.md" "$HOME/.config/tmux/WORKSPACE_GUIDE.md"

if [ -d "$HOME/.omp" ]; then
    cp "$SCRIPT_DIR/omp/tmux-workspace.ts" "$HOME/.omp/agent/extensions/tmux-workspace.ts"
fi

if [ -d "$HOME/.config/nvim" ]; then
    cp "$SCRIPT_DIR/nvim/tmux_workspace.lua" "$HOME/.config/nvim/lua/custom/tmux_workspace.lua"
    if [ -f "$HOME/.config/nvim/init.lua" ] && ! grep -q "custom.tmux_workspace" "$HOME/.config/nvim/init.lua"; then
        echo -e "\nrequire('custom.tmux_workspace').setup()" >> "$HOME/.config/nvim/init.lua"
        echo "    Enabled tmux_workspace in ~/.config/nvim/init.lua"
    fi
fi

# 4. Shell configuration (.zshrc)
echo "--> Checking shell configuration (~/.zshrc)..."
if [ -f "$HOME/.zshrc" ]; then
    if ! grep -q "zoxide init zsh" "$HOME/.zshrc"; then
        echo -e '\neval "$(zoxide init zsh --cmd cd)"' >> "$HOME/.zshrc"
        echo "    Added zoxide init to ~/.zshrc"
    fi
    if ! grep -q "tmux-workspace" "$HOME/.zshrc"; then
        cat << 'EOF' >> "$HOME/.zshrc"

# Project sessions stay opt-in: work opens sesh; work . connects this directory.
work() {
  if [[ "$1" == "rm" || "$1" == "delete" || "$1" == "kill" ]]; then
    shift
    "$HOME/.local/bin/tmux-workspace" kill-workspace "$@"
  else
    "$HOME/.local/bin/tmux-workspace" open "$@"
  fi
}
EOF
        echo "    Added 'work' command to ~/.zshrc"
    fi
fi

# 5. Populate zoxide with projects if present
if command -v zoxide >/dev/null 2>&1 && [ -d "$HOME/untitled folder/Projects" ]; then
    for proj in "$HOME/untitled folder/Projects"/*; do
        if [ -d "$proj" ]; then
            zoxide add "$proj" >/dev/null 2>&1 || true
        fi
    done
fi

# 6. Claude Code integration
if command -v claude >/dev/null 2>&1 && [ -f "$HOME/.claude/settings.json" ]; then
    python3 -c "
import json
p = '$HOME/.claude/settings.json'
try:
    with open(p) as f: d = json.load(f)
    hooks = d.setdefault('hooks', {})
    hook_cmd = '/usr/bin/python3 $HOME/.local/lib/tmux-workspace/agent-hook.py claude'
    for ev in ('SessionStart', 'SessionEnd', 'CwdChanged'):
        arr = hooks.setdefault(ev, [])
        if not any(h.get('command') == hook_cmd for group in arr for h in group.get('hooks', [])):
            arr.append({'hooks': [{'type': 'command', 'command': hook_cmd, 'timeout': 3}]})
    d['statusLine'] = {'type': 'command', 'command': '/usr/bin/python3 $HOME/.local/lib/tmux-workspace/agent-hook.py claude-status', 'refreshInterval': 1}
    with open(p, 'w') as f: json.dump(d, f, indent=2)
    print('    Configured Claude Code lifecycle hooks.')
except Exception as e:
    pass
" 2>/dev/null || true
fi

# 7. Codex integration
if command -v codex >/dev/null 2>&1 && [ -f "$HOME/.codex/config.toml" ]; then
    if ! grep -q 'terminal_title' "$HOME/.codex/config.toml"; then
        echo -e '\n[tui]\nterminal_title = ["app-name", "thread-id"]' >> "$HOME/.codex/config.toml"
        echo "    Configured Codex terminal_title in ~/.codex/config.toml"
    fi
fi

# 8. Reload running tmux if active
if [ -n "${TMUX:-}" ] || tmux has-session 2>/dev/null; then
    tmux source-file "$HOME/.tmux.conf" 2>/dev/null || true
fi

echo ""
echo "================================================================================"
echo "  tmux-workspace installation complete!"
echo "================================================================================"
echo "  Prefix key: Ctrl+a"
echo "  Open workspaces:  work [project-name]"
echo "  Delete workspace: work rm [project-name] (or Ctrl+a X inside tmux)"
echo "  Cheatsheet & Help: Press Ctrl+a ? inside tmux"
echo "  Manual Guide:      less ~/.config/tmux/WORKSPACE_GUIDE.md"
echo "================================================================================"
