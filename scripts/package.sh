#!/bin/sh
# Build the release archive of one target:
#   scripts/package.sh TARGET [OUTDIR]          OUTDIR defaults to dist
#
# Writes OUTDIR/tmux-workspace-TARGET.tar.gz, whose one top folder is tmux-workspace-<version>/, adds
# its line to OUTDIR/SHA256SUMS (sha256sum format) and writes OUTDIR/VERSION, which `install.sh
# --update` reads to learn the latest version. TMUX_WORKSPACE_BINARY packs that binary instead of
# running `cargo build --release --locked --target TARGET` (the tests use the debug build).
set -eu

if [ $# -lt 1 ] || [ $# -gt 2 ] || [ -z "$1" ]; then
    echo 'usage: scripts/package.sh TARGET [OUTDIR]' >&2
    exit 2
fi
target=$1
root=$(cd "$(dirname "$0")/.." && pwd -P)
mkdir -p "${2:-dist}"
out=$(cd "${2:-dist}" && pwd -P)
version=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n 1)
[ -n "$version" ] || { echo "package.sh: no version in $root/Cargo.toml" >&2; exit 1; }

if [ -n "${TMUX_WORKSPACE_BINARY:-}" ]; then
    binary=$TMUX_WORKSPACE_BINARY
else
    cargo build --release --locked --target "$target" --manifest-path "$root/Cargo.toml"
    binary=${CARGO_TARGET_DIR:-$root/target}/$target/release/tmux-workspace
fi
[ -f "$binary" ] || { echo "package.sh: no binary at $binary" >&2; exit 1; }

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
top=tmux-workspace-$version
dir=$stage/$top
mkdir -p "$dir/bin"
cp "$binary" "$dir/bin/tmux-workspace"
cp "$root/bin/tw-clip" "$root/bin/tw-sound" "$dir/bin/"
chmod 755 "$dir/bin/tmux-workspace" "$dir/bin/tw-clip" "$dir/bin/tw-sound"
for file in install.sh tmux.conf agent-status.conf sesh.toml shell.zsh config.example.json \
    README.md LICENSE CHANGELOG.md WORKSPACE_GUIDE.md; do
    cp "$root/$file" "$dir/"
done
chmod 755 "$dir/install.sh"
cp -R "$root/shell" "$root/integrations" "$root/legacy" "$dir/"
# Byte code and Finder files of the working tree are not part of a release.
find "$dir" -name __pycache__ -type d -prune -exec rm -rf {} +
find "$dir" \( -name '*.pyc' -o -name .DS_Store \) -type f -exec rm -f {} +
printf '%s\n' "$version" >"$dir/VERSION"

archive=tmux-workspace-$target.tar.gz
# No macOS metadata (._ files, extended attributes): Linux tar would warn about it.
COPYFILE_DISABLE=1
export COPYFILE_DISABLE
if tar --version 2>/dev/null | grep -q bsdtar; then
    (cd "$stage" && tar --no-xattrs -czf "$out/$archive" "$top")
else
    (cd "$stage" && tar -czf "$out/$archive" "$top")
fi

if command -v sha256sum >/dev/null 2>&1; then
    sum=$(cd "$out" && sha256sum "$archive")
else
    sum=$(cd "$out" && shasum -a 256 "$archive")
fi
# Packaging a target again replaces its line; the lines of other targets stay.
touch "$out/SHA256SUMS"
awk -v name="$archive" '$2 != name && $2 != "*" name' "$out/SHA256SUMS" >"$stage/SHA256SUMS"
printf '%s\n' "$sum" >>"$stage/SHA256SUMS"
cat "$stage/SHA256SUMS" >"$out/SHA256SUMS"
printf '%s\n' "$version" >"$out/VERSION"
echo "$out/$archive ($version)"
