#!/bin/sh
# Install tmux-workspace in fresh Linux containers and smoke-test each one. Needs Docker.
#   tests/install/matrix.sh [image...]
set -u
REPO=$(cd "$(dirname "$0")/../.." && pwd -P)
IMAGES=${*:-ubuntu:24.04 debian:12 fedora:latest archlinux:latest alpine:3.20}
failed=0
for image in $IMAGES; do
    log=$(mktemp)
    platform=
    case $image in archlinux*) platform=--platform=linux/amd64 ;; esac  # Arch publishes amd64 only
    # shellcheck disable=SC2086
    if docker run --rm $platform -v "$REPO":/src:ro "$image" sh /src/tests/install/smoke.sh >"$log" 2>&1; then
        echo "PASS $image"
    else
        echo "FAIL $image"
        sed 's/^/    /' "$log"
        failed=1
    fi
    rm -f "$log"
done
exit $failed
