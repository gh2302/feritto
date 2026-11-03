PREFIX ?= $(HOME)/.local
DESTDIR ?=
CARGO ?= cargo

.PHONY: build check test test-x11 install uninstall
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
install: build
	install -Dm755 target/release/feritto "$(DESTDIR)$(PREFIX)/bin/feritto"
	install -Dm644 assets/desktop/io.github.gh2302.Feritto.desktop "$(DESTDIR)$(PREFIX)/share/applications/io.github.gh2302.Feritto.desktop"
	install -Dm644 assets/icon.svg "$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/io.github.gh2302.Feritto.svg"
	install -Dm644 LICENSE "$(DESTDIR)$(PREFIX)/share/licenses/feritto/LICENSE"
	install -Dm644 NOTICE "$(DESTDIR)$(PREFIX)/share/licenses/feritto/NOTICE"
uninstall:
	rm -f "$(DESTDIR)$(PREFIX)/bin/feritto"
	rm -f "$(DESTDIR)$(PREFIX)/share/applications/io.github.gh2302.Feritto.desktop"
	rm -f "$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/io.github.gh2302.Feritto.svg"
	rm -f "$(DESTDIR)$(PREFIX)/share/licenses/feritto/LICENSE" "$(DESTDIR)$(PREFIX)/share/licenses/feritto/NOTICE"
