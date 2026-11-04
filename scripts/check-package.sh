#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

output=$(realpath "${1:-dist}")
format=${2:-all}
if [[ $format != all && $format != deb ]]; then
    echo "Usage: $0 [output-directory] [all|deb]" >&2
    exit 1
fi
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')
name="feritto-$version-linux-x86_64"
deb_version=${version/-/\~}
deb="$output/feritto_${deb_version}_amd64.deb"
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

(cd "$output" && sha256sum --check SHA256SUMS)
dpkg-deb --extract "$deb" "$temporary/deb"
dpkg-deb --control "$deb" "$temporary/control"
test "$(dpkg-deb --field "$deb" Package)" = feritto
test "$(dpkg-deb --field "$deb" Version)" = "$deb_version"
test "$(dpkg-deb --field "$deb" Architecture)" = amd64
test -n "$(dpkg-deb --field "$deb" Depends)"
test "$(dpkg-deb --field "$deb" Installed-Size)" -gt 0
test "$(dpkg-deb --field "$deb" Homepage)" = https://github.com/gh2302/feritto
dpkg-deb --contents "$deb" | awk '$2 != "root/root" { invalid = 1 } END { exit invalid }'
test "$(stat --format=%a "$temporary/deb/usr")" = 755
test "$(stat --format=%a "$temporary/deb/usr/bin/feritto")" = 755
test "$(stat --format=%a "$temporary/control/control")" = 644
test "$(stat --format=%a "$temporary/control/md5sums")" = 644
(cd "$temporary/deb" && md5sum --check "$temporary/control/md5sums")
test -s "$temporary/deb/usr/share/doc/feritto/copyright"
gzip -t "$temporary/deb/usr/share/doc/feritto/changelog.Debian.gz"
gzip -t "$temporary/deb/usr/share/man/man1/feritto.1.gz"
test ! -e "$temporary/deb/etc/xdg/autostart"
test ! -e "$temporary/deb/usr/lib/systemd"
test -x "$temporary/deb/usr/bin/feritto"
test "$(env -u DISPLAY -u WAYLAND_DISPLAY "$temporary/deb/usr/bin/feritto" --version)" = "feritto $version"
env -u DISPLAY -u WAYLAND_DISPLAY "$temporary/deb/usr/bin/feritto" --help > "$temporary/help"
grep -q 'Usage: feritto' "$temporary/help"
desktop-file-validate "$temporary/deb/usr/share/applications/io.github.gh2302.Feritto.desktop"
cmp docs/debian-ubuntu.md "$temporary/deb/usr/share/doc/feritto/README.Debian"
gzip -dc "$temporary/deb/usr/share/man/man1/feritto.1.gz" | cmp assets/man/feritto.1 -

if [[ $format == deb ]]; then
    printf 'Debian package validation passed\n'
    exit 0
fi

tar -xzf "$output/$name.tar.gz" -C "$temporary"

prefix="$temporary/$name"
for path in bin/feritto share/applications/io.github.gh2302.Feritto.desktop \
    share/icons/hicolor/scalable/apps/io.github.gh2302.Feritto.svg \
    share/licenses/feritto/LICENSE share/licenses/feritto/NOTICE \
    share/doc/feritto/README.md share/doc/feritto/README.Debian \
    share/doc/feritto/io.github.gh2302.Feritto-autostart.desktop share/man/man1/feritto.1.gz; do
    cmp "$prefix/$path" "$temporary/deb/usr/$path"
done
cmp assets/desktop/io.github.gh2302.Feritto.desktop "$prefix/share/applications/io.github.gh2302.Feritto.desktop"
cmp assets/icon.svg "$prefix/share/icons/hicolor/scalable/apps/io.github.gh2302.Feritto.svg"
cmp LICENSE "$prefix/share/licenses/feritto/LICENSE"
cmp NOTICE "$prefix/share/licenses/feritto/NOTICE"
test -x "$prefix/bin/feritto"
test "$(env -u DISPLAY -u WAYLAND_DISPLAY "$prefix/bin/feritto" --version)" = "feritto $version"
desktop-file-validate "$prefix/share/applications/io.github.gh2302.Feritto.desktop"
printf 'Archive and Debian package validation passed\n'
