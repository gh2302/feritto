PREFIX ?= $(HOME)/.local
DESTDIR ?=
CARGO ?= cargo
TARGET_DIR ?= $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)
DIST_DIR ?= dist
DEB_TEST_IMAGES ?= ubuntu:24.04 debian:13-slim

.PHONY: build check test test-x11 test-wayland test-wayland-kwin package deb test-package test-deb test-deb-install install uninstall
build:
	$(CARGO) build --locked --release -p feritto
check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
	$(CARGO) check --locked -p feritto --features wgpu
test:
	$(CARGO) test --workspace --locked
test-x11:
	./scripts/check-x11.sh
test-wayland:
	./scripts/check-wayland.sh sway
test-wayland-kwin:
	./scripts/check-wayland.sh kwin
package: build
	./scripts/package.sh "$(TARGET_DIR)/release/feritto" "$(DIST_DIR)"
deb: build
	./scripts/package.sh "$(TARGET_DIR)/release/feritto" "$(DIST_DIR)" deb
test-package:
	./scripts/check-package.sh "$(DIST_DIR)"
test-deb:
	./scripts/check-package.sh "$(DIST_DIR)" deb
test-deb-install:
	./scripts/check-deb-install.sh "$(DIST_DIR)" $(DEB_TEST_IMAGES)
install: build
	install -Dm755 "$(TARGET_DIR)/release/feritto" "$(DESTDIR)$(PREFIX)/bin/feritto"
	install -Dm644 assets/desktop/io.github.gh2302.Feritto.desktop "$(DESTDIR)$(PREFIX)/share/applications/io.github.gh2302.Feritto.desktop"
	install -Dm644 assets/icon.svg "$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/io.github.gh2302.Feritto.svg"
	install -Dm644 LICENSE "$(DESTDIR)$(PREFIX)/share/licenses/feritto/LICENSE"
	install -Dm644 NOTICE "$(DESTDIR)$(PREFIX)/share/licenses/feritto/NOTICE"
uninstall:
	rm -f "$(DESTDIR)$(PREFIX)/bin/feritto"
	rm -f "$(DESTDIR)$(PREFIX)/share/applications/io.github.gh2302.Feritto.desktop"
	rm -f "$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/io.github.gh2302.Feritto.svg"
	rm -f "$(DESTDIR)$(PREFIX)/share/licenses/feritto/LICENSE" "$(DESTDIR)$(PREFIX)/share/licenses/feritto/NOTICE"
