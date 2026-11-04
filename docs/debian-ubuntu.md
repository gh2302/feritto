# Debian and Ubuntu distribution

feritto is distributed as a standalone `.deb` for **amd64 (x86_64)**. The release
pipeline builds on Ubuntu 24.04 and tests installation on Ubuntu 24.04 and
Debian 13. These are binary distribution packages, not an official Debian/Ubuntu
archive submission, PPA, or APT repository. There is no repository key to add.

The package uses the software renderer and supports both X11 and Wayland. Desktop
and compositor restrictions still apply; in particular, GNOME Wayland does not
expose the clipboard data-control protocols feritto needs. See the main README
for backend capabilities. A passing installation test is not desktop qualification.

## Install a release

Download `feritto_0.1.0_amd64.deb`, the matching `.tar.gz`, and `SHA256SUMS` from
the same GitHub release. In their download directory:

```sh
sha256sum --check SHA256SUMS
sudo apt install ./feritto_0.1.0_amd64.deb
feritto --version
```

Use APT, rather than `dpkg -i`, to resolve runtime dependencies. Rust and the build
tools are not needed on the target machine. Launch **feritto** from the desktop
application menu or run `feritto` in a terminal in your desktop session. Do not
run the application as root. `man feritto` documents the command-line options.

Installing the package does not start clipboard capture, enable autostart, add a
system service, or change an existing user's clipboard history or settings.

For opt-in autostart:

```sh
mkdir -p "$HOME/.config/autostart"
cp /usr/share/doc/feritto/io.github.gh2302.Feritto-autostart.desktop \
  "$HOME/.config/autostart/"
```

Remove that copy to disable autostart. If you previously used `make install`,
run `make uninstall` with the same `PREFIX` before switching to the `.deb`; an
older `~/.local/bin/feritto` may otherwise shadow `/usr/bin/feritto`.

## Upgrade and remove

Quit the currently running instance before installing an update, then restart it
from the desktop application menu:

```sh
feritto --quit
sudo apt install ./feritto_NEW_VERSION_amd64.deb
```

To remove the package:

```sh
feritto --quit
sudo apt remove feritto
rm -f "$HOME/.config/autostart/io.github.gh2302.Feritto-autostart.desktop"
```

Both removal and purge preserve per-user history and settings under
`$XDG_DATA_HOME/feritto` and `$XDG_CONFIG_HOME/feritto` (by default
`~/.local/share/feritto` and `~/.config/feritto`). These files are not owned by the
package. Remove them separately only if you intend to delete your data.
Clipboard history is unencrypted; deletion is not secure erasure.

## Build the distribution locally

Install the Rust toolchain specified in `rust-toolchain.toml` using rustup, then:

```sh
sudo apt-get update
sudo apt-get install build-essential pkg-config libx11-dev libxkbcommon-dev \
  libxkbcommon-x11-0 libfontconfig1-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  dpkg-dev desktop-file-utils python3
make deb
make test-deb
```

Python 3.11+ is required. The native amd64 release build uses the committed
`Cargo.lock` with `--locked`. The first build downloads its locked dependencies;
subsequent builds reuse Cargo's cache. Output is `dist/feritto_0.1.0_amd64.deb`
and `dist/SHA256SUMS`. This checksum file covers the outputs from the most recent
packaging invocation. For both the portable archive and `.deb`, use
`make package && make test-package` instead.

Override `DIST_DIR` and `CARGO_TARGET_DIR` to change output and build locations.
`SOURCE_DATE_EPOCH` defaults to the last Git commit's timestamp and controls
archive metadata, the Debian changelog date, and package timestamps. Packaging
the same binary and inputs with the same epoch produces the same package;
this does not claim that independent compiler builds are bit-for-bit identical.

The package installs the binary under `/usr/bin`, a desktop launcher, a scalable
icon, a compressed manual page, licensing documents, and documentation under
`/usr/share/doc/feritto`. It contains root-owned files without requiring root to
build. Shared-library requirements are detected with `dpkg-shlibdeps`; libraries
loaded dynamically are declared explicitly. `md5sums` enables `dpkg --verify
feritto` to check installed files. SHA-256 checksums verify downloaded artifacts.

Locally built packages inherit the builder's system-library requirements. A
package built on a newer distribution is not necessarily installable on an older
one. Use Ubuntu 24.04 to reproduce the release baseline. Debian 12, Ubuntu 22.04,
ARM, and musl are not prebuilt release targets; build from source as needed.

## Test real package installation

With Docker installed and its daemon accessible:

```sh
make test-deb-install
# Test only one image:
make test-deb-install DEB_TEST_IMAGES=ubuntu:24.04
```

The test runs only in disposable containers, not on your host. It verifies
checksums, resolves dependencies through APT, installs and reinstalls the package,
runs the headless CLI, verifies installed file hashes, then removes and purges
the package while checking that sample user data survives. The default images
are `ubuntu:24.04` and `debian:13-slim`; network access is required for image pulls
and package downloads. The same tests gate CI and tagged releases.
