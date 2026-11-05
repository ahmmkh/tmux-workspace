# Releasing tmux-workspace

For the maintainer: how a release is cut, and how the install and traffic numbers are kept.

## Cut a release

1. On `main`, with CI green, raise `version` in `Cargo.toml` and let cargo update `Cargo.lock`
   (`cargo build`). Commit both: CI builds with `--locked`.
2. Run `make bench` on your Mac. A Rust row over its target is a bug to fix, not a number to print.
   If the numbers moved, update the performance table in `README.md` and the one in the new
   CHANGELOG section.
3. Add `## X.Y.Z — YYYY-MM-DD` above the previous release in `CHANGELOG.md`. The release notes are
   the lines under that heading up to the next `## ` heading, and the publish job fails when the
   section is missing or empty. Write what changed for a user, and any upgrade step.
4. Run `make lint test`, and `make test-install` if you have Docker. Merge to `main` and wait for CI.
5. Tag that commit and push the tag. This is what publishes:

   ```sh
   git tag -a v2.0.0 -m v2.0.0
   git push origin v2.0.0
   ```

A tag with a hyphen (`v2.1.0-rc1`) makes a prerelease, which `releases/latest` never points at, so the
one-line install and `work update` ignore it.

## What `release.yml` does

On a `v*` tag it builds the four archives, then publishes. On a pull request it builds the same four
and publishes nothing, so a broken target shows before the tag.

| Target | Runner |
|---|---|
| `aarch64-apple-darwin` | `macos-latest` |
| `x86_64-apple-darwin` | `macos-latest`, cross-built; the archive is checked, the binary not run |
| `x86_64-unknown-linux-musl` | `ubuntu-latest` |
| `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm` |

Each build runs `scripts/package.sh TARGET`, and on a native runner unpacks the archive and runs its
binary (`help-topic park` must print something). The publish job then:

- merges the four `SHA256SUMS` lines and checks them (`sha256sum -c`), and requires all four archives;
- requires the tag to equal `version` in `Cargo.toml`, and the archives to carry that version;
- takes the release notes from `CHANGELOG.md`, and fails when the section has no text;
- creates the release with `gh release create --verify-tag`, with the four archives, `SHA256SUMS` and
  `VERSION`.

The archives are named `tmux-workspace-<target>.tar.gz` with no version in the name, so
`releases/latest/download/<name>` works without an API call. The version is in the top folder of the
archive and in `VERSION`, which `install.sh --update` reads to learn the latest release.

## If a release goes wrong

- **A build or a check fails, nothing is published.** If it was a runner failure, use *Re-run failed
  jobs* in the Actions tab: the tag still names the same commit. If the code or the CHANGELOG has to
  change, fix `main`, delete the tag (`git push --delete origin vX.Y.Z`, `git tag -d vX.Y.Z`) and tag
  again.
- **Publish fails after it started creating the release.** Look at the Releases page. A release that
  lacks assets must not stay: `gh release delete vX.Y.Z --yes` (this keeps the tag), then re-run the
  publish job. Until you do, `releases/latest` may point at it and installs fail.
- **A published release is bad.** Prefer a new patch release. Deleting the release makes `latest` fall
  back to the one before, but anyone who installed it keeps it, and `work update` offers the next one.

After publishing, install it as a stranger would, with a scratch home, so nothing of yours is touched:

```sh
tmp=$(mktemp -d)
env -i HOME="$tmp" PATH="$PATH" TMUX_TMPDIR="$tmp" bash -c '
  curl -fsSL https://raw.githubusercontent.com/ahmmkh/tmux-workspace/main/install.sh |
    bash -s -- --no-tmux-conf --projects "$HOME/code" &&
  "$HOME/.local/bin/tmux-workspace" doctor'
rm -rf "$tmp"
```

`doctor` warns about the tmux server (there is none in the scratch home), and, if you have an install
of your own, that `tmux-workspace` on `PATH` is not the installed binary. Both are expected here.

## Install and traffic numbers

GitHub keeps 14 days of clone and view counts and no history of stars or downloads.
`.github/workflows/stats.yml` runs `scripts/stats.py record` once a day and commits two CSV files
(`traffic.csv`, `snapshots.csv`) to a private repository, `ahmmkh/tmux-workspace-stats`.
`make stats` reads them.

Set it up once:

1. Create the repository with a first commit, because the workflow checks it out and an empty
   repository has nothing to check out:

   ```sh
   gh repo create ahmmkh/tmux-workspace-stats --private --add-readme
   ```

2. Create a fine-grained personal access token (GitHub settings, Developer settings):
   - resource owner `ahmmkh`;
   - repositories: only `tmux-workspace` and `tmux-workspace-stats`;
   - permissions: **Administration: Read-only** (the traffic numbers need it) and **Contents: Read
     and write** (the workflow pushes the CSV files);
   - an expiry you will remember: when it lapses the workflow fails until you replace the secret.
3. Save it as the Actions secret `STATS_TOKEN` in `tmux-workspace` (Settings, Secrets and variables,
   Actions, or `gh secret set STATS_TOKEN --repo ahmmkh/tmux-workspace`).
4. Run the workflow once from the Actions tab (*Stats*, *Run workflow*). The stats repository gets a
   commit named `Record <date>`.

Without the secret the job only prints a notice, and forks skip it. GitHub disables scheduled
workflows in a repository after 60 days without activity in it: when that happens, enable the
workflow again in the Actions tab. Commits to the stats repository do not count as activity.

Reading the numbers:

```sh
make stats      # downloads by version, clones, views and stars, and the recorded history
```

It uses your own `gh auth login`, which needs access to both repositories. A download is a release
archive (`*.tar.gz`) fetched, so the download count of a release is installs plus updates;
`VERSION` and `SHA256SUMS` are not counted. The first weeks show only the days since the first
record, because GitHub's own traffic window is 14 days.
