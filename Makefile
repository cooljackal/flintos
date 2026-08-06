# SPDX-License-Identifier: Apache-2.0

# Flint RTOS — Developer Makefile
# ================================
# Targets: env, check, build, clean, lint

# ── Default ────────────────────────────────────────────────────────────────────

.DEFAULT_GOAL := help

# ── Configuration ──────────────────────────────────────────────────────────────

# Detect host triple at runtime
HOST_TARGET     := $(shell rustc -vV | sed -n 's/^host: //p')
XTENSA_TARGET   := xtensa-esp32-none-elf
ESP_TOOLCHAIN   := esp
CARGO           := cargo +$(ESP_TOOLCHAIN)

# OS-specific paths (Windows uses USERPROFILE, POSIX uses HOME)
#
# XTENSA_SIZE is passed to tools/image-size.sh explicitly rather than left to
# PATH. On Windows the toolchain goes on PATH as a native path, which the bash
# that runs the script cannot use; forward slashes it can.
ifeq ($(OS),Windows_NT)
  ESP_GCC_DIR   := $(USERPROFILE)\.rustup\toolchains\esp\xtensa-esp-elf\bin
  export XTENSA_SIZE := $(subst \,/,$(USERPROFILE))/.rustup/toolchains/esp/xtensa-esp-elf/bin/xtensa-esp32-elf-size.exe
else
  ESP_GCC_DIR   := $(HOME)/.rustup/toolchains/esp/xtensa-esp-elf/bin
  export XTENSA_SIZE := $(ESP_GCC_DIR)/xtensa-esp32-elf-size
endif

# espflash target/serial parameters (classic ESP32: PICO-D4 and WROVER alike --
# both are esp-idf-format images on the same silicon, so one set of flags
# covers both boards; see the flash-mode note below).
ESPFLASH_CHIP  := esp32
# Flashing (host -> chip) speed. Unrelated to the console baud.
#
# 115200 by default because it is the rate every USB-serial bridge handles.
# espflash itself warns above this ("Setting baud rate higher than 115,200 can
# cause issues"), and it is not an idle warning: at 921600 the connection
# reliably completes the handshake, uploads the flash stub, and then dies with
# "Error while connecting to device" on CP2102/CH340 bridges and through most
# USB hubs. A bring-up default that fails on common hardware is worse than a
# slow one -- the image is ~110 KB, so 115200 costs about ten seconds.
#
# Once flashing works on your board, raise it:
#   make flash-dev FLASH_BAUD=460800
# 460800 is a good middle ground; 921600 works on some FTDI/native-USB setups.
FLASH_BAUD     ?= 115200
# Must match the app's UART0 console baud (board/src/esp32_wrover.rs sets
# 115200). espflash's flash/monitor subcommands take TWO separate baud
# flags -- `--baud`/`-B` is the flashing/sync speed, `--monitor-baud`/`-r` is
# the post-flash serial monitor speed -- do not conflate them, or --monitor
# output after `flash` is unreadable even though flashing itself succeeded.
MONITOR_BAUD   ?= 115200
# DIO is the safe default flash mode for both ESP32-PICO-D4 (embedded flash)
# and ESP32-WROVER (external flash) -- it is also espflash's own default when
# no --flash-mode is given, so this pins that behavior explicitly rather than
# relying on it silently. --flash-freq/--flash-size are deliberately left
# unset: for the `flash` target espflash reads the actual connected chip's
# flash size/frequency over the wire, which is more reliable than hard-coding
# a value that could be wrong for one of the two boards.
FLASH_MODE     := dio

# ── Application selection ──────────────────────────────────────────────────────
#
# The kernel is a library; the binary that gets flashed is an application from
# apps/. Pick one, a board, and how much debug output you want:
#
#   make flash                                    # apps/demo on a WROVER
#   make flash APP=hello                          # apps/hello
#   make flash APP=demo BOARD=board-m5-atom       # M5Stack Atom
#   make flash APP=hello DEBUG=debug-level-0      # no logging at all
#   make apps                                     # what is available
#
# --no-default-features is not optional. Cargo unions features, so without it
# the default board stays enabled alongside the requested one and flint-board's
# compile_error! rejects the build -- deliberately, because a binary with two
# board manifests merged in is not a build for either board.
APP            ?= demo
BOARD          ?= board-esp32-wrover
DEBUG          ?= debug-level-1

APP_FLAGS      := --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins \
                  -p $(APP) --no-default-features --features $(BOARD),$(DEBUG)
APP_BIN        := target/$(XTENSA_TARGET)/debug/$(APP)

# ── Environment Setup ──────────────────────────────────────────────────────────

.PHONY: env
env: ## Install the Espressif Rust Xtensa toolchain (esp-rs/rust-build)
	cargo install espup
	espup install
	$(info )
	$(info === Toolchain installed. Activate with: ===)
	$(info   rustup default $(ESP_TOOLCHAIN))
	$(info   or use:  make env-activate)
	$(info )
	$(info Then build with:  make build)

.PHONY: env-activate
env-activate: ## Source the Espressif environment
ifeq ($(OS),Windows_NT)
	powershell -Command ". $$env:USERPROFILE\export-esp.ps1"
else
	. $(HOME)/export-esp.sh
endif

.PHONY: env-check
env-check: ## Show installed toolchains and targets
	rustup show
	rustup target list --installed

.PHONY: env-uninstall
env-uninstall: ## Remove the Espressif toolchain
	espup uninstall
	rustup toolchain remove $(ESP_TOOLCHAIN)

# ─── Build — Xtensa target ─────────────────────────────────────────────────────

export PATH := $(ESP_GCC_DIR):$(PATH)

.PHONY: build
build: ## Build the selected app (APP=demo BOARD=board-esp32-wrover DEBUG=debug-level-1)
	$(CARGO) build $(APP_FLAGS)
	@bash tools/image-size.sh $(APP_BIN) || true

.PHONY: build-release
build-release: ## Build release (smallest binary)
	$(CARGO) build $(APP_FLAGS) --release
	@bash tools/image-size.sh target/$(XTENSA_TARGET)/release/$(APP) || true

.PHONY: size
size: ## Report where the image's bytes went, per memory region
	@bash tools/image-size.sh $(APP_BIN)

.PHONY: build-trace
build-trace: ## Build with kernel event tracing
	$(MAKE) build DEBUG=debug-level-2

.PHONY: flash
flash: build ## Build + flash + monitor via espflash (USB serial)
	espflash flash $(APP_BIN) \
		--chip $(ESPFLASH_CHIP) --flash-mode $(FLASH_MODE) \
		--baud $(FLASH_BAUD) --monitor --monitor-baud $(MONITOR_BAUD)

.PHONY: flash-dev
flash-dev: flash ## Alias for `flash` (logging is on by default)

.PHONY: flash-jtag
flash-jtag: build ## Build + flash via probe-rs (JTAG)
	probe-rs run --chip ESP32 $(APP_BIN)

.PHONY: apps
apps: ## List the applications in apps/
	@echo "Applications (build with: make flash APP=<name>)"
	@for d in apps/*/; do \
		name=$$(basename $$d); \
		desc=$$(sed -n 's/^description = "\(.*\)"/\1/p' $$d/Cargo.toml); \
		printf "  %-12s %s\n" "$$name" "$$desc"; \
	done
	@echo ""
	@echo "Boards: board-esp32-wrover (default), board-esp32-devkitc, board-m5-atom"
	@echo "Debug:  debug-level-0 (silent) .. debug-level-3 (everything)"

.PHONY: erase
erase: ## Erase the entire flash (recover from a bad/stuck prior image)
	espflash erase-flash --chip $(ESPFLASH_CHIP)

.PHONY: monitor
monitor: ## Open serial monitor (115200 8N1, matches the app console baud)
	espflash monitor --chip $(ESPFLASH_CHIP) --monitor-baud $(MONITOR_BAUD)

# ─── Check (host target — pure Rust only, no Xtensa asm) ───────────────────────

.PHONY: check
check: ## Check all host-compatible crates (flint-hal, flint-api, board, drivers)
	cargo check -p flint-hal --target $(HOST_TARGET)
	cargo check -p flint-api --target $(HOST_TARGET)
	cargo check -p flint-soc-esp32 --target $(HOST_TARGET)
	cargo check -p flint-board --target $(HOST_TARGET)
	cargo check -p esp32-uart --target $(HOST_TARGET)
	cargo check -p esp32-spi --target $(HOST_TARGET)
	cargo check -p esp32-i2c --target $(HOST_TARGET)
	cargo check -p esp32-gpio --target $(HOST_TARGET)
	cargo check -p spi-bus --target $(HOST_TARGET)
	cargo check -p i2c-bus --target $(HOST_TARGET)
	cargo check -p uart-bus --target $(HOST_TARGET)
	cargo check -p bme280 --target $(HOST_TARGET)
	cargo check -p ssd1306 --target $(HOST_TARGET)

.PHONY: check-all
check-all: ## Full check including arch (requires Xtensa toolchain)
	$(CARGO) check --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins

.PHONY: check-layers
check-layers: ## Enforce the three-layer dependency boundary (plan W7.1)
	bash tools/check-layers.sh

.PHONY: test-host
test-host: ## Run host-side tests (hal, api, drivers)
	cargo test -p flint-hal --target $(HOST_TARGET)
	cargo test -p flint-api --target $(HOST_TARGET)
	cargo test -p flint-soc-esp32 --target $(HOST_TARGET)
	cargo test -p flint-board --target $(HOST_TARGET)
	cargo test -p esp32-uart --target $(HOST_TARGET)
	cargo test -p esp32-spi --target $(HOST_TARGET)
	cargo test -p esp32-i2c --target $(HOST_TARGET)
	cargo test -p esp32-gpio --target $(HOST_TARGET)
	cargo test -p spi-bus --target $(HOST_TARGET)
	cargo test -p i2c-bus --target $(HOST_TARGET)
	cargo test -p uart-bus --target $(HOST_TARGET)
	cargo test -p bme280 --target $(HOST_TARGET)
	cargo test -p ssd1306 --target $(HOST_TARGET)

# ─── Lint ───────────────────────────────────────────────────────────────────────

.PHONY: lint
lint: ## Run clippy on all host-checkable crates
	cargo clippy -p flint-hal --target $(HOST_TARGET) -- -D warnings
	cargo clippy -p flint-api --target $(HOST_TARGET) -- -D warnings

# ─── Info ───────────────────────────────────────────────────────────────────────

.PHONY: info
info: ## Show tracked files and total size
	@echo "Tracked files (excluding target/):"; git ls-files | grep -v target/ | wc -l; echo "Total workspace size:"; du -sh .

# ─── Clean ──────────────────────────────────────────────────────────────────────

.PHONY: clean
clean: ## Remove all build artifacts
	cargo clean

# ─── Help ───────────────────────────────────────────────────────────────────────

.PHONY: help
help: ## Show this help message
	$(info Flint RTOS — Make targets:)
	$(info )
	$(info   Environment)
	$(info     make env            Install Xtensa toolchain via espup)
	$(info     make env-activate   Source the Espressif environment)
	$(info     make env-check      Show installed toolchains)
	$(info     make env-uninstall  Remove Espressif toolchain)
	$(info )
	$(info   Build)
	$(info     make build          Debug build for ESP32)
	$(info     make build-release  Release build (minimal binary))
	$(info     make build-trace    Build with kernel event tracing)
	$(info     make build-dev      Build with logging (debug level 1))
	$(info     make flash          Build + flash via espflash (USB serial))
	$(info     make flash-dev      Flash + monitor the logging build)
	$(info     make flash-jtag     Build + flash via probe-rs (JTAG))
	$(info     make erase          Erase entire flash (recover from bad image))
	$(info     make monitor        Open serial monitor)
	$(info )
	$(info   Check / Test)
	$(info     make check          Check host crates (flint-hal, flint-api))
	$(info     make check-all      Full check including arch)
	$(info     make test-host      Run host-side unit tests)
	$(info )
	$(info   Quality)
	$(info     make lint           Run clippy on host crates)
	$(info     make info           Show project file tree)
	$(info     make clean          Remove build artifacts)
	$(info )
	$(info   Quick start:  make env  ->  make check  ->  make build)
