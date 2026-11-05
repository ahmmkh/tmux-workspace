#!/bin/sh
# Install tmux-workspace in fresh Linux containers from a release archive and smoke-test each one.
# Needs Docker.
#   tests/install/matrix.sh [image...]
# TMUX_WORKSPACE_DIST names a folder with the release archives and SHA256SUMS, as scripts/package.sh
# writes them. Without it, the archive for Docker's architecture is built first, in rust:1-alpine.
set -u
REPO=$(cd "$(dirname "$0")/../.." && pwd -P)
IMAGES=${*:-ubuntu:24.04 debian:12 fedora:latest archlinux:latest alpine:3.20}

case $(docker info --format '{{.Architecture}}' 2>/dev/null) in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *)
        echo 'matrix.sh: cannot tell the architecture of Docker' >&2
        exit 1
        ;;
esac

if [ -n "${TMUX_WORKSPACE_DIST:-}" ]; then
    dist=$(cd "$TMUX_WORKSPACE_DIST" && pwd -P)
else
    dist=$(mktemp -d)
    trap 'rm -rf "${dist:?}"' EXIT
    echo "== building the $arch-unknown-linux-musl archive"
    # The cargo registry and target folder live in a volume, so the next run builds only what changed.
    if ! docker run --rm -v "$REPO":/src:ro -v "$dist":/dist -v tmux-workspace-cargo:/cache \
        -e CARGO_HOME=/cache/cargo -e CARGO_TARGET_DIR=/cache/target -w /src rust:1-alpine \
        sh -c "apk add -q musl-dev && scripts/package.sh $arch-unknown-linux-musl /dist && chown -R $(id -u):$(id -g) /dist"; then
        echo 'FAIL building the archive'
        exit 1
    fi
fi

failed=0
for image in $IMAGES; do
    need=$arch
    platform=
    case $image in archlinux*) need=x86_64 platform=--platform=linux/amd64 ;; esac # Arch publishes amd64 only
    if [ ! -f "$dist/tmux-workspace-$need-unknown-linux-musl.tar.gz" ]; then
        echo "SKIP $image (no $need archive)"
        continue
    fi
    log=$(mktemp)
    # shellcheck disable=SC2086
    if docker run --rm $platform -v "$REPO":/src:ro -v "$dist":/dist:ro -e TMUX_WORKSPACE_DIST=/dist \
        "$image" sh /src/tests/install/smoke.sh >"$log" 2>&1; then
        echo "PASS $image"
    else
        echo "FAIL $image"
        sed 's/^/    /' "$log"
        failed=1
    fi
    rm -f "$log"
done
exit $failed
