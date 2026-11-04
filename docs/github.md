# GitHub CI and release setup

feritto is a Linux desktop application. Deployment means distributing tested
desktop packages through GitHub Releases, not deploying a server. The configured
remote is `git@github.com:gh2302/feritto.git`.

## First push and repository settings

Create the empty GitHub repository if it does not exist, review and commit the
changes, and push the `main` branch:

```sh
git push -u origin main
```

The CI workflow runs on pull requests, pushes to `main`, merge-queue
checks, and manual dispatch. If the default branch is renamed to anything else,
update the push filter in `.github/workflows/ci.yml`.

Configure these settings in GitHub after the first successful run:

1. Enable GitHub Actions and permit the pinned `actions/*` and
   `Swatinem/rust-cache` actions. Keep the default workflow token permissions
   read-only; only the release publishing job requests `contents: write`.
2. Protect the default branch with a ruleset requiring pull requests and the
   **Required checks** status. That aggregate check fails if any validation job
   fails, is skipped, or is cancelled. Require an up-to-date branch or use the
   merge queue; disable force pushes and branch deletion. Choose review
   requirements appropriate for the number of maintainers.
3. Protect release tags matching `v*`: restrict tag creation to maintainers and
   prevent updates and deletion. Do not move an already published version tag.
4. Create the `github-releases` environment, restrict deployments to tags matching
   `v*`, and optionally require a maintainer approval before publication. Without
   approval rules, a passing tag run publishes automatically.
5. Enable Dependabot alerts and security updates. The checked-in Dependabot
   configuration also proposes weekly Cargo and GitHub Actions version updates.

Rulesets, environment protections, and Actions policies are repository settings;
checking in workflow files alone does not enable them. Availability of individual
protections depends on the repository visibility and GitHub plan.

No repository secrets, personal access token, registry account, or hosting
credentials are required. GitHub supplies the short-lived `GITHUB_TOKEN`.
Pull-request jobs have read-only permissions and cannot publish a release.

## Validation pipeline

All builds use Ubuntu 24.04, the toolchain from `rust-toolchain.toml`, and the
committed `Cargo.lock` with `--locked`:

| Job | Checks |
| --- | --- |
| Formatting and lint | SHA-verified actionlint, ShellCheck for packaging scripts, rustfmt, Clippy with denied warnings, optional wgpu build |
| Workspace tests | All workspace unit and non-desktop integration tests |
| Desktop (x11) | Isolated Xvfb protocol tests and i3/Xfwm smoke tests against the packaged release binary |
| Desktop (wayland) | Private headless sway protocol tests and smoke tests against the packaged release binary |
| Linux release packages | Optimized binary, archive and Debian package, checksums, payload comparison, desktop launcher validation, headless CLI checks |
| Debian/Ubuntu installation | APT install, reinstall, remove and purge on Ubuntu 24.04 and Debian 13 containers, installed-file verification and user-data preservation |
| Required checks | Requires every preceding job to succeed |

The desktop jobs download and verify the package artifact rather than rebuilding
the application, so the binary exercised by smoke tests is the one published.
The Debian/Ubuntu jobs install that same `.deb` in clean containers, resolving
runtime dependencies from each distribution's repositories.
Jobs have explicit timeouts. Desktop matrix entries both finish even if one
fails. Superseded branch/PR runs are cancelled, while release runs are not.
Only pushes to the default branch can save Rust caches. Action dependencies are
pinned to full commit SHAs. The actionlint download is pinned by version and
SHA-256; update both together when upgrading it.

The `feritto-linux-x86_64` artifact contains the packages and checksums and is
retained for 14 days. It is available on branch/PR runs even before the first
release. Existing limitations in desktop qualification still apply; passing
automation is not a claim of full desktop/compositor compatibility.

## Make a release

1. Change `[workspace.package].version` in `Cargo.toml`. Refresh the lockfile with
   `cargo check --workspace`, review its changes, and commit the manifest and
   lockfile together through the normal pull-request process.
2. Merge to the default branch and wait for CI to pass. Update the local branch
   before tagging the intended commit.
3. Create and push an annotated tag whose name is exactly `v` plus the workspace
   version. For the current version:

   ```sh
   git switch main
   git pull --ff-only
   git tag -a v0.1.0 -m "feritto 0.1.0"
   git push origin v0.1.0
   ```

4. The release workflow rejects mismatched versions and commits not on the
   default branch. It runs the same reusable CI workflow, downloads its validated
   package artifact, and checks SHA-256 again. It creates a draft with generated
   release notes, uploads the files, then publishes only after uploads succeed.
   Approve the environment deployment if approval is configured.

Tags containing a prerelease suffix, such as `v0.2.0-rc.1`, publish as GitHub
prereleases and must likewise match the workspace version. The Debian package
uses `~` for the prerelease separator so release candidates sort before the final
version. No `cargo publish` runs; workspace crates are not published to crates.io.

Failed validation publishes nothing. If an upload fails, the release stays a
draft, and rerunning the failed publishing job can replace its draft assets.
The workflow refuses to overwrite an already published release. Fix the problem
and create a new version rather than moving a published tag.

## Packages and installation

Published files for version `0.1.0` are:

- `feritto-0.1.0-linux-x86_64.tar.gz`
- `feritto_0.1.0_amd64.deb`
- `SHA256SUMS`

Download all three into one directory and verify before installing:

```sh
sha256sum --check SHA256SUMS
sudo apt install ./feritto_0.1.0_amd64.deb
```

The Debian package installs under `/usr`; its linked-library dependencies are
generated by `dpkg-shlibdeps`, with explicit dependencies for libraries loaded at
runtime. For a per-user install instead:

```sh
tar -xzf feritto-0.1.0-linux-x86_64.tar.gz
mkdir -p "$HOME/.local"
cp -a feritto-0.1.0-linux-x86_64/. "$HOME/.local/"
```

Ensure `~/.local/bin` is on the desktop session's `PATH`. Both formats include
the desktop launcher, icon, license, notice, README, manual page, and an optional autostart
desktop file under `share/doc/feritto/`. Neither enables autostart automatically,
starts the service during installation, or changes clipboard/history data.

CI packages target x86_64 GNU/Linux, built against Ubuntu 24.04's system
libraries. They are not static or universal Linux binaries: use Ubuntu 24.04 or
a compatible newer distribution (such as Debian 13). Older glibc distributions,
musl, ARM, Windows, and macOS are not release targets. Build from source for other
Linux environments. The release uses the default software renderer; the optional
wgpu renderer is compile-checked, not shipped as a separate package.

To reproduce the packaging workflow on a Debian/Ubuntu x86_64 host with the
normal build dependencies installed:

```sh
sudo apt-get install dpkg-dev desktop-file-utils python3
make package
make test-package
```

Python 3.11 or newer is required for its standard-library TOML parser. Override
`DIST_DIR` to choose an output directory and `CARGO_TARGET_DIR` to choose a build
directory. `SOURCE_DATE_EPOCH` defaults to the last commit timestamp and controls
package timestamps. Packages built locally inherit the local distribution's
linked-library requirements.

Use `make deb && make test-deb` to build and validate only the `.deb`.
`make test-deb-install` additionally uses Docker to test installation,
reinstallation, removal and purge on Ubuntu 24.04 and Debian 13. See
[Debian/Ubuntu distribution](debian-ubuntu.md) for the complete installation,
upgrade, removal and local-build guide.
