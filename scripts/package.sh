#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

binary=$(realpath "${1:-target/release/feritto}")
output=${2:-dist}
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')
if [[ $(uname -m) != x86_64 || $(dpkg --print-architecture) != amd64 ]]; then
    echo "Release packaging currently supports native Linux x86_64 builds only" >&2
    exit 1
fi
if [[ $("$binary" --version) != "feritto $version" ]]; then
    echo "The release binary does not match workspace version $version" >&2
    exit 1
fi
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}
mkdir -p "$output"
output=$(realpath "$output")
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

name="feritto-$version-linux-x86_64"
prefix="$temporary/$name"
install -Dm755 "$binary" "$prefix/bin/feritto"
install -Dm644 assets/desktop/io.github.gh2302.Feritto.desktop \
    "$prefix/share/applications/io.github.gh2302.Feritto.desktop"
install -Dm644 assets/icon.svg "$prefix/share/icons/hicolor/scalable/apps/io.github.gh2302.Feritto.svg"
install -Dm644 LICENSE "$prefix/share/licenses/feritto/LICENSE"
install -Dm644 NOTICE "$prefix/share/licenses/feritto/NOTICE"
install -Dm644 README.md "$prefix/share/doc/feritto/README.md"
install -Dm644 assets/desktop/io.github.gh2302.Feritto-autostart.desktop \
    "$prefix/share/doc/feritto/io.github.gh2302.Feritto-autostart.desktop"
tar --sort=name --mtime="@$SOURCE_DATE_EPOCH" --owner=0 --group=0 --numeric-owner \
    -C "$temporary" -cf - "$name" | gzip -n > "$output/$name.tar.gz"

package="$temporary/debian/feritto"
mkdir -p "$package/DEBIAN" "$package/usr"
cp -a "$prefix/." "$package/usr/"
cat > "$temporary/debian/control" <<'CONTROL'
Source: feritto
Section: utils
Priority: optional
Maintainer: gh2302 <336852531+gh2302@users.noreply.github.com>

Package: feritto
Architecture: amd64
Description: Native X11 and Wayland clipboard history manager
CONTROL
dependencies=$(cd "$temporary" && dpkg-shlibdeps --package=feritto -O -edebian/feritto/usr/bin/feritto)
dependencies=${dependencies#shlibs:Depends=}
deb_version=${version/-/\~}
cat > "$package/DEBIAN/control" <<CONTROL
Package: feritto
Version: $deb_version
Section: utils
Priority: optional
Architecture: amd64
Maintainer: gh2302 <336852531+gh2302@users.noreply.github.com>
Depends: $dependencies, libfontconfig1, libx11-6, libxcursor1, libxi6, libxrandr2, libxkbcommon0, libxkbcommon-x11-0, libxcb1, libxcb-render0, libxcb-shape0, libxcb-xfixes0, libwayland-client0, libwayland-cursor0
Recommends: fonts-dejavu-core
Description: Native X11 and Wayland clipboard history manager
 Capture, search and reuse clipboard history with a native desktop popup.
CONTROL
deb_name="feritto_${deb_version}_amd64.deb"
dpkg-deb --root-owner-group --build "$package" "$output/$deb_name"
(cd "$output" && sha256sum "$name.tar.gz" "$deb_name" > SHA256SUMS)
printf 'Release packages written to %s\n' "$output"
