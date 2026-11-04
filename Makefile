PYTHON ?= python3
RUNTIME := bin/tmux-workspace bin/install-integrations $(wildcard lib/*.py) integrations/agent-hook.py $(wildcard scripts/*.py)
SHELL_SCRIPTS := install.sh bin/tw-clip bin/tw-sound shell/work.sh tests/install/smoke.sh tests/install/matrix.sh

.PHONY: test lint test-install screenshots

test:
	$(PYTHON) -m unittest discover -s tests -p 'test_*.py'

lint:
	shellcheck $(SHELL_SCRIPTS)
	bash -n install.sh
	for f in $(RUNTIME); do $(PYTHON) -m py_compile "$$f" || exit 1; done

# Fresh Linux containers; pick some with: make test-install IMAGES="debian:12 alpine:3.20"
test-install:
	tests/install/matrix.sh $(IMAGES)

screenshots:
	$(PYTHON) scripts/demo.py
