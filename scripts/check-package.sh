#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

output=$(realpath "${1:-dist}")
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')
name="feritto-$version-linux-x86_64"
deb_version=${version/-/\~}
deb="$output/feritto_${deb_version}_amd64.deb"
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

(cd "$output" && sha256sum --check SHA256SUMS)
tar -xzf "$output/$name.tar.gz" -C "$temporary"
dpkg-deb --extract "$deb" "$temporary/deb"
test "$(dpkg-deb --field "$deb" Package)" = feritto
test "$(dpkg-deb --field "$deb" Version)" = "$deb_version"
test "$(dpkg-deb --field "$deb" Architecture)" = amd64
test -n "$(dpkg-deb --field "$deb" Depends)"

prefix="$temporary/$name"
for path in bin/feritto share/applications/io.github.gh2302.Feritto.desktop \
    share/icons/hicolor/scalable/apps/io.github.gh2302.Feritto.svg \
    share/licenses/feritto/LICENSE share/licenses/feritto/NOTICE \
    share/doc/feritto/README.md share/doc/feritto/io.github.gh2302.Feritto-autostart.desktop; do
    cmp "$prefix/$path" "$temporary/deb/usr/$path"
done
cmp assets/desktop/io.github.gh2302.Feritto.desktop "$prefix/share/applications/io.github.gh2302.Feritto.desktop"
cmp assets/icon.svg "$prefix/share/icons/hicolor/scalable/apps/io.github.gh2302.Feritto.svg"
cmp LICENSE "$prefix/share/licenses/feritto/LICENSE"
cmp NOTICE "$prefix/share/licenses/feritto/NOTICE"
test -x "$prefix/bin/feritto"
test -x "$temporary/deb/usr/bin/feritto"
test "$(env -u DISPLAY -u WAYLAND_DISPLAY "$prefix/bin/feritto" --version)" = "feritto $version"
test "$(env -u DISPLAY -u WAYLAND_DISPLAY "$temporary/deb/usr/bin/feritto" --version)" = "feritto $version"
desktop-file-validate "$prefix/share/applications/io.github.gh2302.Feritto.desktop"
printf 'Archive and Debian package validation passed\n'
