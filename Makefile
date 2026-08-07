# SPDX-License-Identifier: Apache-2.0

# Flint RTOS — Developer Makefile
# ================================
# Targets: env, check, build, clean, lint

# ── Default ────────────────────────────────────────────────────────────────────

.DEFAULT_GOAL := help

# ── Configuration ──────────────────────────────────────────────────────────────

# Detect the host triple. `rustc --print host-tuple` rather than piping
# `rustc -vV` through sed: make's $(shell) does not get a pipeline through
# reliably on every Windows shell, and when it fails the variable is empty and
# every `cargo --target $(HOST_TARGET)` dies with "--target takes a target
# architecture as an argument" -- a message that points nowhere near the cause.
#
# `?=` rather than `:=` so a caller can override it from the environment. CI
# sets HOST_TARGET explicitly; with `:=` make would silently ignore that and
# substitute its own detection, which happens to agree on the runner and so
# would hide the override rather than honour it.
HOST_TARGET     ?= $(shell rustc --print host-tuple)
XTENSA_TARGET   := xtensa-esp32-none-elf
ESP_TOOLCHAIN   := esp
CARGO           := cargo +$(ESP_TOOLCHAIN)

# Windows detection, which is not just `$(OS)`.
#
# `$(OS)` is set only when make inherits a native Windows environment. MSYS2's
# make -- what Git Bash and the MSYS2 shell both provide -- strips OS,
# USERPROFILE, TMP and TEMP from the environment it passes on. Testing OS alone
# therefore takes the POSIX branch on a Windows box, silently -- the temp fix
# below never runs, and every link fails for a reason that names no cause.
# MSYSTEM is set by every MSYS2 shell.
ifeq ($(OS),Windows_NT)
  WINDOWS       := 1
else ifneq ($(MSYSTEM),)
  WINDOWS       := 1
endif

# Where the Xtensa gcc lives, asked rather than guessed.
#
# This used to be built from `$(USERPROFILE)` or `$(HOME)` plus `/.rustup`, and
# it was wrong on both Windows shells: MSYS2 strips USERPROFILE, and its $(HOME)
# is the MSYS home (/home/<user>), while rustup installs under the Windows
# profile. `rustup show home` is the only thing that actually knows, and it
# honours RUSTUP_HOME for anyone who has moved it.
#
# Forward slashes even on Windows: this ends up on PATH for a gcc invoked
# through cargo, and the shells in between do not agree on backslashes.
RUSTUP_HOME_DIR := $(subst \,/,$(shell rustup show home 2>/dev/null))
ifeq ($(RUSTUP_HOME_DIR),)
  RUSTUP_HOME_DIR := $(if $(WINDOWS),$(subst \,/,$(USERPROFILE)),$(HOME))/.rustup
endif

# The bash that runs tools/*.sh. Never bare `bash`.
#
# On Windows, C:\Windows\System32\bash.exe is the WSL launcher and sits ahead of
# MSYS2 and Git Bash on a default PATH. A recipe saying `bash` therefore runs
# the script inside a Linux distro that has no Rust toolchain, no espflash, and
# no access to COM ports -- and the failure blames the toolchain rather than the
# shell. Which bash wins also depends on the shell make was invoked from, so the
# same target worked from Git Bash and failed from PowerShell on one machine.
#
# Prefer the bash that ships beside the make reading this file. `wildcard`
# resolves /usr/bin/bash to bash.exe on MSYS2, and the path exists on Linux;
# macOS has /bin/bash instead, where the PATH lookup is correct anyway.
BASH := $(if $(wildcard /usr/bin/bash),/usr/bin/bash,bash)

ESP_GCC_DIR    := $(RUSTUP_HOME_DIR)/toolchains/esp/xtensa-esp-elf/bin

# Where cargo put its binaries -- cargo, rustup, espflash all live here.
#
# Needed because the PATH make hands to a recipe does not reliably contain it.
# cargo adds ~/.cargo/bin to the *persisted* Windows PATH, so a shell opened
# before the install does not have it; and MSYS2's make strips USERPROFILE, so
# a recipe cannot reconstruct the path either -- $(HOME) there is the MSYS home
# (/home/<user>), not the Windows profile where cargo actually installed.
#
# `rustup show home` is the one thing that reports the Windows profile
# correctly, and .cargo is its sibling under a default install. CARGO_HOME wins
# if it is set, for anyone who moved it.
CARGO_BIN_DIR  := $(if $(CARGO_HOME),$(subst \,/,$(CARGO_HOME))/bin,$(patsubst %/.rustup,%/.cargo/bin,$(RUSTUP_HOME_DIR)))

# Restore TMP/TEMP for the processes make runs.
#
# MSYS2's make drops both, so every linker reached from a recipe -- link.exe for
# host tests, xtensa-esp32-elf-gcc for firmware -- falls back to C:\WINDOWS,
# which is not writable. It surfaces as `LNK1104: cannot open file
# C:\WINDOWS\lnk{...}.tmp` or `Cannot create temporary file in C:\WINDOWS\:
# Permission denied`, neither of which names the environment as the cause.
#
# These are native tools, so they need a native path: `cygpath -w /tmp` gives
# one (MSYS2 maps /tmp to the Windows temp directory). `?=` so a caller can
# override, and so native-Windows make -- which does inherit TMP -- keeps its
# own. Nothing here runs on POSIX, where the linker finds /tmp by itself.
ifdef WINDOWS
  WIN_TMP       := $(shell cygpath -w /tmp 2>/dev/null)
  ifneq ($(WIN_TMP),)
    TMP         ?= $(WIN_TMP)
    TEMP        ?= $(TMP)
    export TMP
    export TEMP
  endif
endif

# The memory map, read by `make size` to name and bound each region.
LD_SCRIPT      := arch/xtensa/flint32.ld

# Every workspace member builds and tests on the host EXCEPT these three, so name
# the exceptions rather than listing the other sixteen. A crate added tomorrow is
# covered the day it lands, instead of being silently skipped until someone
# remembers to add it here; a crate that genuinely needs the Xtensa toolchain has
# to declare itself, once, with a reason.
#
#   arch-xtensa        #![feature(asm_experimental_arch)] -- E0554 on stable
#   hello, demo        binaries: they need the linker script and a memory map
#
# The kernel is deliberately NOT excluded any more. It reaches the machine only
# through `kernel::arch`, which stands in for it on a host, and its dependency
# on arch-xtensa is scoped to `cfg(target_os = "none")` -- so `cargo test -p
# kernel` builds here and its unit tests run on every change, like any other
# crate's. Before that seam existed they ran nowhere at all.
#
# This replaces a hand-kept list of fifteen names, which lived here and in three
# more copies in ci.yml. Those copies had already drifted once.
HOST_EXCLUDE   := arch-xtensa hello demo blink
HOST_SELECT    := --workspace $(addprefix --exclude ,$(HOST_EXCLUDE))

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
# Serial port. Empty lets espflash auto-detect, which is right when exactly one
# board is attached. With more than one it prompts, so name the port:
#   make flash PORT=COM5          (Windows)
#   make flash PORT=/dev/ttyUSB0  (Linux)
PORT           ?=
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
#   make flash APP=blink BOARD=board-m5-atom-matrix  # apps/blink, 5x5 panel
#   make flash APP=demo BOARD=board-m5-atom-lite     # M5Stack Atom Lite
#   make flash APP=hello DEBUG=debug-level-0      # no logging at all
#   make apps                                     # what is available
#
# --no-default-features is not optional. Cargo unions features, so without it
# the default board stays enabled alongside the requested one and the board
# crate's compile_error! rejects the build -- deliberately, because a binary
# with two board manifests merged in is not a build for either board.
APP            ?= demo
BOARD          ?= board-esp32-wrover
DEBUG          ?= debug-level-1

# Anything else the app forwards, comma-separated. Currently just:
#   make build EXTRA_FEATURES=self-test   # boot-time register-window check
# `make upgrade` only: skip the pull and check what is already checked out.
PULL           ?= 1

EXTRA_FEATURES ?=

COMMA          := ,
APP_FEATURES   := $(BOARD),$(DEBUG)$(if $(EXTRA_FEATURES),$(COMMA)$(EXTRA_FEATURES))
APP_FLAGS      := --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins \
                  -p $(APP) --no-default-features --features $(APP_FEATURES)
APP_BIN        := target/$(XTENSA_TARGET)/debug/$(APP)

# ── Environment Setup ──────────────────────────────────────────────────────────

##@ Environment
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

# Both directories go on the PATH every recipe sees. ESP_GCC_DIR supplies the
# Xtensa gcc that cargo invokes as a linker; CARGO_BIN_DIR supplies cargo,
# rustup and espflash themselves, which a terminal opened before the toolchain
# was installed will not have. Without the latter, `make build` and
# `make test-target` fail with "not found" in one terminal and work in another,
# and the message blames this file rather than the session.
export PATH := $(ESP_GCC_DIR):$(CARGO_BIN_DIR):$(PATH)

##@ Build and flash
.PHONY: build
build: ## Build the selected app (APP=demo BOARD=board-esp32-wrover DEBUG=debug-level-1)
	$(CARGO) build $(APP_FLAGS)
	@$(MAKE) --no-print-directory size

.PHONY: build-release
build-release: ## Build release (smallest binary)
	$(CARGO) build $(APP_FLAGS) --release
	@$(MAKE) --no-print-directory size APP_BIN=target/$(XTENSA_TARGET)/release/$(APP)

.PHONY: size
size: ## Report where the image's bytes went, per memory region
	@cargo run -q -p size --target $(HOST_TARGET) -- $(APP_BIN) $(LD_SCRIPT)

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
	@echo "Boards: board-esp32-wrover (default), board-esp32-devkitc, board-m5-atom-lite, board-m5-atom-matrix"
	@echo "Debug:  debug-level-0 (silent) .. debug-level-3 (everything)"

.PHONY: erase
erase: ## Erase the entire flash (recover from a bad/stuck prior image)
	espflash erase-flash --chip $(ESPFLASH_CHIP)

.PHONY: monitor
monitor: ## Open serial monitor (115200 8N1, matches the app console baud)
	espflash monitor --chip $(ESPFLASH_CHIP) --monitor-baud $(MONITOR_BAUD)

# ─── Check (host target — pure Rust only, no Xtensa asm) ───────────────────────

##@ Check and test
.PHONY: check
check: ## Check every host-compatible crate
	cargo check $(HOST_SELECT) --target $(HOST_TARGET)

.PHONY: check-all
check-all: ## Full check including arch (requires Xtensa toolchain)
	$(CARGO) check --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins \
		--workspace --exclude build --exclude size

.PHONY: check-layers
check-layers: ## Enforce the three-layer dependency boundary (plan W7.1)
	$(BASH) tools/check-layers.sh

.PHONY: device-matrix
device-matrix: ## Which drivers keep which device-class promises
	@$(BASH) tools/check-devices.sh

.PHONY: check-names
check-names: ## Enforce the package naming and layout convention
	$(BASH) tools/check-names.sh

.PHONY: test-host
test-host: test-boards ## Run host-side unit tests (every board manifest included)
	cargo test $(HOST_SELECT) --target $(HOST_TARGET)

# Every board this tree can build for. A manifest's invariant tests only run
# for the board that is selected, so testing the default board alone leaves
# every other manifest unchecked -- which is how a pin or a panel layout stays
# wrong until someone flashes it.
BOARDS := board-esp32-wrover board-esp32-devkitc board-m5-atom-lite board-m5-atom-matrix

.PHONY: test-boards
test-boards: ## Run each board manifest's invariant tests, one board at a time
	@for b in $(BOARDS); do echo "== $$b"; cargo test -p board --target $(HOST_TARGET) --no-default-features --features "$$b" || exit 1; done

# ── On-target tests ───────────────────────────────────────────────────────────
#
# These need a board, so they are asked for rather than run automatically. They
# cover what no host test can: register windows spilled across a trap, a
# critical section that genuinely masks the timer, a tick driven by silicon.
# See kernel/src/selftest.rs.

#   make test-target BOARD=board-m5-atom-matrix PORT=COM5
#
# PORT matters as soon as a second serial device is attached: espflash prompts
# for a choice, and the harness has no terminal to answer with.
.PHONY: test-target
test-target: ## Flash and run the on-target self-tests (needs a board attached)
	APP="$(APP)" BOARD="$(BOARD)" DEBUG="$(DEBUG)" PORT="$(PORT)" \
	FLASH_BAUD="$(FLASH_BAUD)" MONITOR_BAUD="$(MONITOR_BAUD)" \
	ESPFLASH_CHIP="$(ESPFLASH_CHIP)" FLASH_MODE="$(FLASH_MODE)" \
	$(BASH) tools/target-test.sh

# The judging half of the harness, checked without hardware. It is the part
# ── Watchdog verification ─────────────────────────────────────────────────────
#
# A watchdog that is armed but does not fire is worse than none: it reports
# protection nobody has. The only way to know is to break the board on purpose
# and watch it come back.
#
#   make test-watchdog WDT=kernel   # mask interrupts and hang -> RTC WDT, ~5 s
#   make test-watchdog WDT=idle     # spin without yielding    -> idle WDT, ~10 s
#
# PASS is the board rebooting: the ROM banner and `FlintMain reached` appear a
# second time, a few seconds after the [wdt-test] line. FAIL is the console
# going quiet and staying quiet -- that means armed-but-not-firing, which is
# the outcome worth finding.
WDT ?= kernel

.PHONY: test-watchdog
test-watchdog: ## Prove a watchdog actually resets the board (WDT=kernel|idle)
	@echo "Flashing demo with watchdog-test-$(WDT)."
	@echo "PASS = the board reboots a few seconds after the [wdt-test] line."
	@echo "FAIL = the console goes quiet and stays quiet."
	@echo
	$(MAKE) flash EXTRA_FEATURES=watchdog-test-$(WDT)

# ── Upgrading ─────────────────────────────────────────────────────────────────
#
# Applications are separate crates, so a pull never touches apps/<yours>/. What
# it does not do by itself is tell you what changed underneath them.
#
#   make upgrade            # pull, rebuild every app, report what broke
#   make upgrade PULL=0     # check against what is already checked out
.PHONY: upgrade
upgrade: ## Pull the latest Flint and report which applications it broke
	@PULL="$(PULL)" BOARD="$(BOARD)" DEBUG="$(DEBUG)" \
	CARGO="$(CARGO)" XTENSA_TARGET="$(XTENSA_TARGET)" \
	$(BASH) tools/upgrade.sh

# most likely to be wrong and the most expensive to exercise for real, and a
# harness that calls a dropped serial line a pass is worse than none.
.PHONY: test-harness
test-harness: ## Test the on-target harness's judging logic (no board needed)
	$(BASH) tools/target-test-selftest.sh

# ─── Lint ───────────────────────────────────────────────────────────────────────

##@ Quality
.PHONY: lint
lint: ## Run clippy on every host crate, tests included, warnings denied
	cargo clippy $(HOST_SELECT) --target $(HOST_TARGET) \
		--all-targets -- -D warnings

# ─── Info ───────────────────────────────────────────────────────────────────────

##@ Misc
.PHONY: info
info: ## Show tracked files and total size
	@echo "Tracked files (excluding target/):"; git ls-files | grep -v target/ | wc -l; echo "Total workspace size:"; du -sh .

# ─── Clean ──────────────────────────────────────────────────────────────────────

.PHONY: clean
clean: ## Remove all build artifacts
	cargo clean

# ─── Help ───────────────────────────────────────────────────────────────────────

# Derived from the `## ` docstring on each target, not written out by hand.
#
# The hand-written version had already drifted: it listed neither test-target,
# test-harness nor check-names, so the three newest targets were undiscoverable
# from the one place people look for them. A list kept in lock-step by hand with
# the thing it describes eventually describes something else.
#
# The `##@ Group` lines elsewhere in this file set the section headings.
.PHONY: help
help: ## Show this help message
	@printf 'Flint RTOS — Make targets:\n'
	@awk 'BEGIN { FS = ":.*## " } \
	     /^##@ / { printf "\n  %s\n", substr($$0, 5); next } \
	     /^[a-zA-Z0-9_-]+:.*## / { printf "    make %-16s %s\n", $$1, $$2 }' \
	     $(MAKEFILE_LIST)
	@printf '\n  Quick start:   make env  ->  make check  ->  make build\n'
	@printf '  Host tests:    make test-host    (no hardware; runs in CI)\n'
	@printf '  Board tests:   make test-target BOARD=board-m5-atom-matrix PORT=COM5\n\n'
