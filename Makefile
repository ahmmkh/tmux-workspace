PYTHON ?= python3
RUNTIME := legacy/bin/tmux-workspace $(wildcard legacy/lib/*.py) legacy/integrations/agent-hook.py integrations/agent-hook.py $(wildcard scripts/*.py)
SHELL_SCRIPTS := install.sh bin/tw-clip bin/tw-sound shell/work.sh scripts/package.sh scripts/bench.sh tests/install/smoke.sh tests/install/matrix.sh
# The release target of this machine: Linux releases are static musl builds.
TARGET ?= $(shell rustc -vV 2>/dev/null | sed -n 's/^host: //p' | sed 's/-gnu$$/-musl/')

.PHONY: build test lint dist test-install screenshots stats bench

# The debug build, in Cargo's target folder, which the tests run. Nothing here installs it: in a clone,
# ./install.sh builds the release binary into bin/tmux-workspace (gitignored) and swaps it in at once.
build:
	cargo build

# make test never writes bin/tmux-workspace: in a clone that is also the live install, tmux runs it in
# every new pane. Its checksum from before the run (empty when it is absent) is checked at the end.
test: LIVE_BINARY := $(shell cksum bin/tmux-workspace 2>/dev/null)
test: build
	cargo test --features testing
	$(PYTHON) -m unittest discover -s tests -p 'test_*.py'
	@[ "$$(cksum bin/tmux-workspace 2>/dev/null)" = "$(LIVE_BINARY)" ] || \
		{ echo 'make test: bin/tmux-workspace changed during the run' >&2; exit 1; }

lint:
	shellcheck $(SHELL_SCRIPTS)
	bash -n install.sh
	for f in $(RUNTIME); do $(PYTHON) -m py_compile "$$f" || exit 1; done
	cargo clippy --all-targets --features testing -- -D warnings
	cargo fmt --check
	@if command -v actionlint >/dev/null 2>&1; then echo actionlint; actionlint; \
	else echo 'make lint: actionlint is not installed, so .github/workflows is not checked'; fi

# The release archive of this machine in dist/: make dist TARGET=x86_64-apple-darwin for another one.
dist:
	scripts/package.sh $(TARGET) dist

# Fresh Linux containers; pick some with: make test-install IMAGES="debian:12 alpine:3.20"
test-install:
	tests/install/matrix.sh $(IMAGES)

# Runs ./install.sh of this checkout in a sandbox HOME, which builds the release binary into bin/.
screenshots:
	$(PYTHON) scripts/demo.py

# The five paths this program made fast, timed against the Python they replaced (needs hyperfine).
bench:
	scripts/bench.sh

# Downloads by version, clones, views and stars. Reads GitHub with your `gh auth login`; the history
# is what .github/workflows/stats.yml records daily into the private ahmmkh/tmux-workspace-stats.
stats:
	$(PYTHON) scripts/stats.py show --history ahmmkh/tmux-workspace-stats
