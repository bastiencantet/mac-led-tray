.PHONY: build run setup app dmg clean

HELPER = target/release/led-helper
APP    = target/release/mac-led-tray

build:
	cargo build --release --bin led-helper
	cargo build --release --bin mac-led-tray

# One-time: make the helper setuid root so SMC writes work without sudo.
# Only needed during dev — the bundled app auto-elevates on first launch.
setup: build
	@echo "Installing led-helper with admin privileges..."
	osascript -e "do shell script \"chown root '$(CURDIR)/$(HELPER)' && chmod u+s '$(CURDIR)/$(HELPER)'\" with administrator privileges"
	@echo "Done. Run: make run"

run: build
	$(APP)

# Build the LED.app bundle under dist/
app:
	./scripts/bundle.sh

# Build the LED.app and package it as dist/LED-<version>.dmg
dmg:
	./scripts/dmg.sh

clean:
	cargo clean
	rm -rf dist
