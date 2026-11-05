PYTHON ?= python3
RUNTIME := legacy/bin/tmux-workspace $(wildcard legacy/lib/*.py) legacy/integrations/agent-hook.py integrations/agent-hook.py $(wildcard scripts/*.py)
SHELL_SCRIPTS := install.sh bin/tw-clip bin/tw-sound shell/work.sh scripts/package.sh tests/install/smoke.sh tests/install/matrix.sh
# The release target of this machine: Linux releases are static musl builds.
TARGET ?= $(shell rustc -vV 2>/dev/null | sed -n 's/^host: //p' | sed 's/-gnu$$/-musl/')

.PHONY: build test lint dist test-install screenshots stats

# bin/tmux-workspace is the debug build, copied there (it is gitignored): install.sh links it, and the
# Python tests and the demo run it from that path. The old file goes first: macOS can kill a signed
# binary that was overwritten in place.
build:
	cargo build
	rm -f bin/tmux-workspace
	install -m 755 target/debug/tmux-workspace bin/tmux-workspace

test: build
	cargo test --features testing
	$(PYTHON) -m unittest discover -s tests -p 'test_*.py'

lint:
	shellcheck $(SHELL_SCRIPTS)
	bash -n install.sh
	for f in $(RUNTIME); do $(PYTHON) -m py_compile "$$f" || exit 1; done
	cargo clippy --all-targets --features testing -- -D warnings
	cargo fmt --check

# The release archive of this machine in dist/: make dist TARGET=x86_64-apple-darwin for another one.
dist:
	scripts/package.sh $(TARGET) dist

# Fresh Linux containers; pick some with: make test-install IMAGES="debian:12 alpine:3.20"
test-install:
	tests/install/matrix.sh $(IMAGES)

screenshots: build
	$(PYTHON) scripts/demo.py

# Downloads by version, clones, views and stars. Reads GitHub with your `gh auth login`; the history
# is what .github/workflows/stats.yml records daily into the private ahmmkh/tmux-workspace-stats.
stats:
	$(PYTHON) scripts/stats.py show --history ahmmkh/tmux-workspace-stats
