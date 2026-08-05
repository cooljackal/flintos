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
ifeq ($(OS),Windows_NT)
  ESP_GCC_DIR   := $(USERPROFILE)\.rustup\toolchains\esp\xtensa-esp-elf\bin
else
  ESP_GCC_DIR   := $(HOME)/.rustup/toolchains/esp/xtensa-esp-elf/bin
endif

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
build: ## Build for Xtensa ESP32 (requires Xtensa toolchain)
	$(CARGO) build --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins

.PHONY: build-release
build-release: ## Build release (all debug off, smallest binary)
	$(CARGO) build --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins --release

.PHONY: build-trace
build-trace: ## Build with kernel event tracing (debug level 2)
	$(CARGO) build --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins --features "debug-level-2"

.PHONY: flash
flash: build ## Build + flash via espflash (USB serial)
	espflash flash target/$(XTENSA_TARGET)/debug/flint-kernel --baud 921600 --monitor

.PHONY: build-dev
build-dev: ## Build with logging on (needed to SEE the demo task output at G1)
	$(CARGO) build --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins --features debug-level-1

.PHONY: flash-dev
flash-dev: build-dev ## Flash + monitor the logging build (ESP32-PICO/ATOM, USB serial)
	espflash flash target/$(XTENSA_TARGET)/debug/flint-kernel --baud 921600 --monitor

.PHONY: flash-jtag
flash-jtag: build ## Build + flash via probe-rs (JTAG)
	probe-rs run --chip ESP32 target/$(XTENSA_TARGET)/debug/flint-kernel

.PHONY: monitor
monitor: ## Open serial monitor (115200 8N1)
	espflash monitor --baud 115200

# ─── Check (host target — pure Rust only, no Xtensa asm) ───────────────────────

.PHONY: check
check: ## Check all host-compatible crates (flint-hal, flint-api, board, drivers)
	cargo check -p flint-hal --target $(HOST_TARGET)
	cargo check -p flint-api --target $(HOST_TARGET)
	cargo check -p flint-board --target $(HOST_TARGET)
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
