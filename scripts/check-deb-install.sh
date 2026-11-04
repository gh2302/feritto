#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

output=$(realpath "${1:-dist}")
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')
deb_version=${version/-/\~}
deb_name="feritto_${deb_version}_amd64.deb"
test -f "$output/$deb_name"
if [[ $# -gt 0 ]]; then
    shift
fi
images=("$@")
if [[ ${#images[@]} -eq 0 ]]; then
    images=(ubuntu:24.04 debian:13-slim)
fi
(cd "$output" && sha256sum --check SHA256SUMS)

for image in "${images[@]}"; do
    printf 'Checking Debian package installation on %s\n' "$image"
    docker run --rm --interactive --platform linux/amd64 \
        --mount "type=bind,source=$output,target=/packages,readonly" \
        --tmpfs /var/lib/apt/lists --tmpfs /var/cache/apt/archives \
        --env DEBIAN_FRONTEND=noninteractive \
        "$image" bash -s -- "$deb_name" "$version" <<'CONTAINER'
set -euo pipefail
trap 'printf "Installation check failed: %s\n" "$BASH_COMMAND" >&2' ERR
deb_name=$1
version=$2
printf '%s\n' \
    'path-include=/usr/share/doc/feritto' \
    'path-include=/usr/share/doc/feritto/*' \
    'path-include=/usr/share/man/man1' \
    'path-include=/usr/share/man/man1/feritto.1.gz' \
    > /etc/dpkg/dpkg.cfg.d/zz-feritto-test
mkdir -p /home/feritto-test/.local/share/feritto
printf 'keep history\n' > /home/feritto-test/.local/share/feritto/package-test
apt-get update
apt-get install --yes --no-install-recommends "/packages/$deb_name"
test "$(dpkg-query --show --showformat='${Status}' feritto)" = 'install ok installed'
test "$(env -u DISPLAY -u WAYLAND_DISPLAY feritto --version)" = "feritto $version"
env -u DISPLAY -u WAYLAND_DISPLAY feritto --help > /tmp/feritto-help
grep -q 'Usage: feritto' /tmp/feritto-help
dpkg --verify feritto > /tmp/feritto-verify
test ! -s /tmp/feritto-verify
test -s /usr/share/man/man1/feritto.1.gz
test -s /usr/share/doc/feritto/copyright
test -s /usr/share/doc/feritto/README.Debian
test ! -e /etc/xdg/autostart/io.github.gh2302.Feritto-autostart.desktop
apt-get install --yes --no-install-recommends --reinstall "/packages/$deb_name"
apt-get remove --yes feritto
test ! -e /usr/bin/feritto
test ! -e /usr/share/applications/io.github.gh2302.Feritto.desktop
test ! -e /usr/share/icons/hicolor/scalable/apps/io.github.gh2302.Feritto.svg
test ! -e /usr/share/man/man1/feritto.1.gz
test -s /home/feritto-test/.local/share/feritto/package-test
apt-get install --yes --no-install-recommends "/packages/$deb_name"
apt-get purge --yes feritto
test ! -e /usr/bin/feritto
test -s /home/feritto-test/.local/share/feritto/package-test
printf 'Install, reinstall, remove and purge checks passed\n'
CONTAINER
done
