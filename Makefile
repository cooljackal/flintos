# SPDX-License-Identifier: Apache-2.0

# FlintOS — Developer Makefile
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
ARM_TARGET      := thumbv6m-none-eabi
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

# Python, for the tooling that needs more than shell. Same probing reason
# as the checkers: Windows ships a `python3` shim that is not an
# interpreter.
PY := $(shell for c in python3 python py; do if command -v $$c >/dev/null 2>&1 && $$c -c '' >/dev/null 2>&1; then echo $$c; break; fi; done)

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
# The template, and the resolved copy `build::link()` leaves behind. The two
# are identical unless a radio feature moved the memory map, so `size` prefers
# the resolved one and falls back to the template before the first build.
LD_TEMPLATE    := arch/xtensa/flint32.ld
LD_GENERATED   := target/flint32.generated.ld
LD_SCRIPT       = $(if $(wildcard $(LD_GENERATED)),$(LD_GENERATED),$(LD_TEMPLATE))

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
# more copies in ci.yml. Those copies had already drifted once. The
# applications are derived from the tree: every directory under
# apps/examples/ and apps/tests/ is a package named after its leaf. The
# `patsubst` strips the trailing slash `wildcard` leaves on a directory
# pattern -- plain `$(notdir)` on `apps/examples/hello/` yields nothing.
HOST_EXCLUDE   := arch-xtensa $(notdir $(patsubst %/,%,$(wildcard apps/*/*/)))
HOST_SELECT    := --workspace $(addprefix --exclude ,$(HOST_EXCLUDE))

# The host suite still has to compile `board` and `kernel`, and both need a
# manifest. This names one for them.
#
# It is not a default board: nothing here is flashed, and `make test-boards`
# runs every manifest's invariant tests one board at a time regardless of what
# this says. It exists so the workspace build has *a* pin map, and naming it in
# one place beats scattering `--features` across four recipes.
# `=` not `:=`: HOST_BOARD is defined further down, and immediate expansion
# here would resolve it to nothing.
HOST_BOARD_FEATURES = --features board/$(HOST_BOARD),kernel/$(HOST_BOARD)

# espflash target/serial parameters (classic ESP32: PICO-D4 and WROVER alike --
# both are esp-idf-format images on the same silicon, so one set of flags
# covers both boards; see the flash-mode note below).
ESPFLASH_CHIP  := esp32
# Flashing (host -> chip) speed. Unrelated to the console baud.
#
# 460800 by default: it is esp-idf's own default upload rate, roughly 4x faster
# than 115200, and completes reliably on the common bridges (verified on a
# DevKitC over CP2102). espflash still warns above 115200 ("Setting baud rate
# higher than 115,200 can cause issues") -- the warning is expected, not a
# failure.
#
# Faster and slower, both documented fallbacks:
#   make flash FLASH_BAUD=921600   # Arduino's default; works on FTDI/native-USB
#                                  # and this DevKitC, but has been seen to sync
#                                  # then die with "Error while connecting to
#                                  # device" on some CP2102/CH340 bridges + hubs
#   make flash FLASH_BAUD=115200   # the universal fallback if a board won't sync
#
# espflash skips writing any bootloader/partition-table/app region whose
# checksum already matches what is on the chip (its default; `--no-skip` is the
# opt-out), so an unchanged rebuild reflashes only the segments that changed --
# there is nothing to configure here for that.
FLASH_BAUD     ?= 460800
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
# apps/examples/ or apps/tests/. Pick one, a board, and how much debug output
# you want:
#
#   make flash                                    # apps/examples/demo on a WROVER
#   make flash APP=hello                          # apps/examples/hello
#   make flash APP=blink BOARD=board-m5-atom-matrix  # apps/examples/blink, 5x5 panel
#   make flash APP=demo BOARD=board-m5-atom-lite     # M5Stack Atom Lite
#   make flash APP=hello DEBUG=debug-level-0      # no logging at all
#   make apps                                     # what is available
#
# --no-default-features is kept even though no board is a default feature any
# more: it keeps the feature set to exactly what is named here, so a debug
# level added to some crate's defaults later cannot quietly change a build.
#
# **BOARD has no default and must be given.** A board manifest is the pin map,
# the bus map and the IRQ numbers; defaulting it means flashing a board you did
# not choose, and the default that used to be here was the one board nobody had
# ever flashed. `require-board` below fails with the list rather than picking.
APP            ?= demo
BOARD          ?=
DEBUG          ?= debug-level-1

# Which manifest the *host* suite compiles against. Not a default board: no
# host test flashes anything, and `make test-boards` runs every manifest's
# invariant tests one board at a time regardless of this. Something has to be
# selected for `board` and `kernel` to compile at all on the host, and this
# names it in one place instead of scattering it.
HOST_BOARD     ?= board-esp32-devkitc

# Anything else the app forwards, comma-separated. Currently just:
#   make build EXTRA_FEATURES=self-test   # boot-time register-window check
# `make upgrade` only: skip the pull and check what is already checked out.
PULL           ?= 1

EXTRA_FEATURES ?=

COMMA          := ,
# Board and debug level belong to the kernel, and cargo accepts `pkg/feature`
# on the command line for a workspace member -- so they are passed as
# `kernel/board-x,kernel/debug-level-n` rather than as features the app
# re-declares and forwards. That is what let the ~25-line forwarding block leave
# every app manifest (#120). EXTRA_FEATURES is appended verbatim, so it carries
# whatever the app still owns (self-test, blobs, watchdog-test-*, radio-bt) or a
# further kernel feature (`kernel/radio-ble`).
APP_FEATURES   := kernel/$(BOARD),kernel/$(DEBUG)$(if $(EXTRA_FEATURES),$(COMMA)$(EXTRA_FEATURES))
ifeq ($(BOARD),board-wio-rp2040-mini)
CARGO          := cargo
APP_FLAGS      := --target $(ARM_TARGET) -p $(APP) --no-default-features --features $(APP_FEATURES)
APP_BIN        := target/$(ARM_TARGET)/debug/$(APP)
APP_UF2        := target/$(ARM_TARGET)/debug/$(APP).uf2
APP_RAW_BIN    := target/$(ARM_TARGET)/debug/$(APP).bin
else
APP_FLAGS      := --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins \
                  -p $(APP) --no-default-features --features $(APP_FEATURES)
APP_BIN        := target/$(XTENSA_TARGET)/debug/$(APP)
endif

# Developer-local secrets, sourced into the environment of the build recipes.
# `wificonnect`'s build.rs reads FLINT_WIFI_SSID / FLINT_WIFI_PASS through
# `option_env!`, so they must be exported for the `$(CARGO) build` that compiles
# it. Kept in a git-ignored `.env` rather than a shell profile so a checkout
# carries its own network without touching the environment.
#
# Sourced by the shell, never `include`d as a makefile: a passphrase may hold
# `$`, `#` or spaces, which make would mangle but `.` reads literally. Format is
# KEY=VALUE, one per line, unquoted (the shell keeps quotes verbatim). `set -a`
# exports whatever the file defines; a missing file is a silent no-op, so a
# build that needs no credentials is unaffected.
LOAD_ENV := if [ -f .env ]; then set -a; . ./.env; set +a; fi;

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
# Refuse to guess a board.
#
# Printed rather than defaulted, because the failure a default produces is
# silent: the build succeeds, the image flashes, and the first symptom is a
# peripheral on the wrong pin. One word on the command line is cheaper than
# that. The list is ordered by how well tested each board is, and says so.
# Refuse to guess a board.
#
# Reported through make's own $(error) rather than a shell recipe: it fires
# before anything is built, needs no escaping, and cannot be defeated by the
# `-k` flag. Only for goals that actually produce an image -- `make help`,
# `make test-host` and `make apps` have no business demanding one.
#
# A default here would be a board you flash without choosing it, and the
# default that used to be here was the one board nobody had ever flashed.
define BOARD_HELP

No board selected, and there is no default.

  make <target> BOARD=<board>

    board-esp32-devkitc     ESP32-DevKitC / WROOM-32   verified on hardware
    board-wio-rp2040-mini   Seeed Wio RP2040 Mini      bring-up in progress
    board-m5-atom-matrix    M5Stack Atom Matrix        verified on hardware
    board-m5-atom-lite      M5Stack Atom Lite          verified on hardware
    board-m5-core2          M5Stack Core2              bring-up in progress
    board-esp32-wrover      ESP32-WROVER               never flashed

endef

BOARD_GOALS := build build-release flash flash-dev flash-release test-target test-watchdog check-features
ifneq ($(filter $(BOARD_GOALS),$(MAKECMDGOALS)),)
  ifeq ($(strip $(BOARD)),)
    $(error $(BOARD_HELP))
  endif
endif
build: ## Build the selected app (APP=demo BOARD=board-esp32-devkitc DEBUG=debug-level-1)
	@$(LOAD_ENV) $(CARGO) build $(APP_FLAGS)
ifeq ($(BOARD),board-wio-rp2040-mini)
	pwsh -NoProfile -File tools/rp2040-image.ps1 -Action convert \
		-Architecture armv6m -Soc rp2040 -Board wio-rp2040-mini \
		-Elf $(APP_BIN) -Bin $(APP_RAW_BIN) -Uf2 $(APP_UF2)
else
	@$(MAKE) --no-print-directory size
endif

.PHONY: build-release
build-release: ## Build release (smallest binary)
	@$(LOAD_ENV) $(CARGO) build $(APP_FLAGS) --release
	@$(MAKE) --no-print-directory size APP_BIN=target/$(XTENSA_TARGET)/release/$(APP)

.PHONY: size
size: ## Report where the image's bytes went, per memory region
	@cargo run -q -p size --target $(HOST_TARGET) -- $(APP_BIN) $(LD_SCRIPT)

.PHONY: build-trace
build-trace: ## Build with kernel event tracing
	$(MAKE) build DEBUG=debug-level-2

.PHONY: flash
flash: build ## Build + flash + monitor via espflash (USB serial)
ifeq ($(BOARD),board-wio-rp2040-mini)
	pwsh -NoProfile -File tools/rp2040-image.ps1 -Action flash \
		-Architecture armv6m -Soc rp2040 -Board wio-rp2040-mini -Uf2 $(APP_UF2)
else
	espflash flash $(APP_BIN) \
		--chip $(ESPFLASH_CHIP) --flash-mode $(FLASH_MODE) \
		--baud $(FLASH_BAUD) --monitor --monitor-baud $(MONITOR_BAUD)
endif

.PHONY: flash-dev
flash-dev: flash ## Alias for `flash` (logging is on by default)

.PHONY: flash-jtag
flash-jtag: build ## Build + flash via probe-rs (JTAG)
	probe-rs run --chip ESP32 $(APP_BIN)

.PHONY: apps
apps: ## List the applications in apps/examples/ and apps/tests/
	@echo "Applications (build with: make flash APP=<name>)"
	@for group in apps/examples apps/tests; do \
		echo "$$group/"; \
		for d in $$group/*/; do \
			name=$$(basename $$d); \
			desc=$$(sed -n 's/^description = "\(.*\)"/\1/p' $$d/Cargo.toml); \
			printf "  %-12s %s\n" "$$name" "$$desc"; \
		done; \
	done
	@echo ""
	@echo "Boards: $(BOARDS)   (first is the default)"
	@echo "Debug:  debug-level-0 (silent) .. debug-level-3 (everything)"

# ── Scaffolding ───────────────────────────────────────────────────────────────
#
# Copy-a-template chores, automated. `new-app` and `add-driver` create a crate;
# `drivers` is the catalog; `enable-driver`/`disable-driver` toggle a driver
# dependency in an app (thin wrappers over `cargo add`/`cargo remove`).

.PHONY: new-app
new-app: ## Scaffold a new app: make new-app NAME=<name> [DESC="..."]
	@NAME="$(NAME)" DESC="$(DESC)" $(BASH) tools/new-app.sh

.PHONY: drivers
drivers: ## List drivers: make drivers [CATEGORY=physical|bus|logical] [MATCH=<pat>]
	@CATEGORY="$(CATEGORY)" MATCH="$(MATCH)" $(BASH) tools/drivers.sh

.PHONY: enable-driver
enable-driver: ## Depend on a driver in an app: make enable-driver APP=<app> DRIVER=<name>
	@APP="$(APP)" DRIVER="$(DRIVER)" $(BASH) tools/enable-driver.sh

.PHONY: disable-driver
disable-driver: ## Drop a driver from an app: make disable-driver APP=<app> DRIVER=<name>
	@APP="$(APP)" DRIVER="$(DRIVER)" $(BASH) tools/disable-driver.sh

.PHONY: add-driver
add-driver: ## Scaffold a new driver crate: make add-driver NAME=<name> [CATEGORY=physical|bus|logical]
	@NAME="$(NAME)" CATEGORY="$(CATEGORY)" SOC="$(SOC)" DESC="$(DESC)" $(BASH) tools/add-driver.sh

# Interpolated rather than spelled out, because the hand-kept copy that used to
# be here is exactly the list `test-boards` walks: a board in one and not the
# other is a board whose manifest invariants never run.

.PHONY: blobs blob-symbols blobs-check erase
blobs: ## Fetch Espressif's radio blobs (Apache-2.0, ~4 MB, pinned to esp-idf v4.4)
	@$(BASH) tools/fetch-blobs.sh

blob-symbols: ## List what the radio blobs need and no blob provides (step 3.3)
	@ESP_GCC_DIR="$(ESP_GCC_DIR)" $(PY) tools/blob-symbols.py $(if $(ELF),$(ELF),)

blobs-check: ## Report whether the radio blobs have been fetched
	@$(BASH) tools/fetch-blobs.sh --check

erase: ## Erase the entire flash (recover from a bad/stuck prior image)
	espflash erase-flash --chip $(ESPFLASH_CHIP)

.PHONY: monitor
monitor: ## Open serial monitor (115200 8N1, matches the app console baud)
	espflash monitor --chip $(ESPFLASH_CHIP) --monitor-baud $(MONITOR_BAUD)

# ─── Check (host target — pure Rust only, no Xtensa asm) ───────────────────────

##@ Check and test
.PHONY: check
check: ## Check every host-compatible crate
	cargo check $(HOST_SELECT) --target $(HOST_TARGET) $(HOST_BOARD_FEATURES)

# rustdoc for the user-facing API, generated from the doc comments so it cannot
# drift from the code. Same host-crate set as `check`/`test-host` (HOST_SELECT),
# so the API reference covers exactly what builds on the host: the `api` system
# surface, the bus/driver traits in `hal`, the portable `lib/*` crates, and the
# logical drivers. Output lands in `target/$(HOST_TARGET)/doc`.
#
# The *published* reference is now `apidoc` (#132) on flintos.dev, not this; the
# GitHub Pages build that used to publish this rustdoc was retired with #129.
# `docs` stays as a local sanity check that the doc comments still build --
# their prose is the source `apidoc` renders, and it must not rot.
.PHONY: docs
docs: ## Build rustdoc locally (a doc-comment sanity check; the site is `apidoc`)
	cargo doc --no-deps $(HOST_SELECT) --target $(HOST_TARGET) $(HOST_BOARD_FEATURES)

# The site API reference (#132): rustdoc's JSON output, rendered by tools/apidoc
# into Starlight pages so the reference lives inside flintos.dev -- the site's
# theme and, the point, its Pagefind search -- and still cannot drift from the
# code. Supersedes the standalone rustdoc site (`docs`).
#
# Two steps: emit one <crate>.json per crate (nightly-only format, gated on
# stable with RUSTC_BOOTSTRAP=1, pinned to FORMAT_VERSION 57 by tools/apidoc's
# rustdoc-types dep), then render. The host tool crates are excluded -- they are
# not part of the public API a reader wants.
#
# Output lands in a git-ignored content dir; it is a build artifact regenerated
# in CI before the site build, never committed.
APIDOC_OUT     := site/src/content/docs/api
API_SELECT     := $(HOST_SELECT) --exclude build --exclude size --exclude apidoc
# An isolated target dir: the JSON pass and the ordinary embedded build must not
# share a cache, or the other's artifacts (built with a different core) leak in.
API_TARGET_DIR := target/apidoc
API_JSON_DIR   := $(API_TARGET_DIR)/$(HOST_TARGET)/doc
# The JSON output format is nightly-only, so rustdoc runs with RUSTC_BOOTSTRAP=1
# -- but only rustdoc, via this wrapper, never cargo. If cargo saw it too it
# would honour `[unstable] build-std` from .cargo/config.toml and build a second
# `core` for the host target, which collides with the sysroot's (E0152). Cargo
# on stable ignores build-std and documents against sysroot core cleanly.
# Key off the host triple, not $(OS): under MSYS make, $(OS) is not reliably
# "Windows_NT". cargo (a native binary) cannot exec a shell script as RUSTDOC on
# Windows, so hand it the .bat via a native (cygpath -w) path.
ifneq (,$(findstring windows,$(HOST_TARGET)))
RUSTDOC_WRAP := $(shell cygpath -w $(CURDIR)/tools/rustdoc-json.bat)
else
RUSTDOC_WRAP := $(CURDIR)/tools/rustdoc-json.sh
endif
.PHONY: apidoc
apidoc: ## Generate the site API reference (Starlight pages) from rustdoc JSON
	CARGO_TARGET_DIR=$(API_TARGET_DIR) RUSTDOC='$(RUSTDOC_WRAP)' \
		RUSTDOCFLAGS='--output-format json -Z unstable-options' \
		cargo doc --no-deps $(API_SELECT) --target $(HOST_TARGET) $(HOST_BOARD_FEATURES)
	cargo run -q -p apidoc --target $(HOST_TARGET) -- $(API_JSON_DIR) $(APIDOC_OUT)

# Applications that refuse to build for the default board, and the board each
# one wants. `blink`, `imu` and `pwm` need hardware only the Atoms declare, and
# they say so with a `compile_error!` naming the board -- which is good
# behaviour that made this target permanently red, because a plain `--workspace`
# builds every app against the default WROVER. It had been failing for exactly
# that reason, which is the trouble with a check nobody can ever see pass.
#
# So: everything else against the default, then each of these against the board
# it asks for. Coverage goes up, not down -- before this they were excluded from
# the Xtensa check by failing it.
# Which manifest the workspace-wide Xtensa check compiles against. As with
# HOST_BOARD, not a default board: it is the pin map the crates that need
# one get checked against, and CI builds every application for every board
# separately. The board-specific apps below override it with the one they
# require.
XTENSA_BOARD   ?= board-esp32-devkitc
# One board feature, on the kernel, unified across the whole workspace check.
# The apps no longer carry a board feature to forward (#120): each reaches the
# board through its `kernel` dependency, and cargo unifies `kernel/board-x` over
# every member being checked. `board/x` is listed too so the `board` crate --
# also a member, and guarded to refuse building with no board -- is selected
# explicitly rather than only transitively.
XTENSA_BOARD_FEATURES = --features board/$(XTENSA_BOARD),kernel/$(XTENSA_BOARD)

BOARD_SPECIFIC_APPS := blink imu pwm
ATOM_BOARD          := board-m5-atom-matrix
# The M5Core2 bring-up apps guard on manifest features only that board declares
# (`backlight` on `BOARD.pmic`, `lcd` on `BOARD.display`), so like the Atom apps
# they cannot build against the default board and are checked against theirs.
CORE2_APPS          := backlight lcd
CORE2_BOARD         := board-m5-core2

.PHONY: check-all
check-all: ## Full check including Xtensa and ARM architectures
	$(CARGO) check --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins \
		--workspace --exclude build --exclude size --exclude apidoc \
		$(addprefix --exclude ,$(BOARD_SPECIFIC_APPS)) \
		$(addprefix --exclude ,$(CORE2_APPS)) \
		$(XTENSA_BOARD_FEATURES)
	@for a in $(BOARD_SPECIFIC_APPS); do \
		echo "== $$a ($(ATOM_BOARD))"; \
		$(CARGO) check --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins \
			-p $$a --no-default-features \
			--features "kernel/$(ATOM_BOARD),kernel/debug-level-1" || exit 1; \
	done
	@for a in $(CORE2_APPS); do \
		echo "== $$a ($(CORE2_BOARD))"; \
		$(CARGO) check --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins \
			-p $$a --no-default-features \
			--features "kernel/$(CORE2_BOARD),kernel/debug-level-1" || exit 1; \
	done
	@echo "== arm-selftest (board-wio-rp2040-mini)"
	@# Plain `cargo`, not $(CARGO): the ARM port builds on the stable toolchain
	@# with a prebuilt core, so this needs `rustup target add $(ARM_TARGET)`
	@# on that toolchain rather than build-std. ci.yml installs it.
	cargo check --target $(ARM_TARGET) -p arm-selftest --no-default-features \
		--features "kernel/board-wio-rp2040-mini,kernel/debug-level-1"

# Feature combinations that gate real code, and which nothing else builds.
#
# `check-all` builds the workspace with *default* features, and `lint` runs on
# the host — so anything behind a non-default feature, or behind
# `target_os = "none"`, is checked by neither. `kernel::selftest` is both:
# `pub mod selftest` is gated on `all(feature = "self-test", target_os =
# "none")`, so no host clippy can ever see it however the features are set.
#
# That was not theoretical. A `spin_ticks` helper went in with the module path
# written as `super::selftest::spin_ticks` instead of `super::spin_ticks`, and
# `make test-host`, `make lint` and `make check-all` were all green. It was
# found by flashing a board, which is an expensive compiler.
#
# One entry per combination that turns code on, not a powerset. `debug-level-0`
# earns its place by turning things *off*: it is where an unused import or a
# variable only read by a log line becomes an error.
# Each entry is the exact feature string to add on top of the board and
# debug-level-1, since board and debug are kernel features now (#120) and the
# radio modes live on the kernel too. `self-test`, `radio-bt` and the
# `watchdog-test-*` pair are demo's own features and stay bare; the rest are
# `kernel/...`. `radio-ble` reserves BT DRAM through demo's `radio-bt` (which
# `build::link()` reads) and selects the mode with `kernel/radio-ble`, exactly
# as the old app-level `radio-ble` did.
FEATURE_CHECKS := \
	self-test \
	kernel/debug-level-0 \
	kernel/debug-level-3 \
	radio-bt \
	radio-bt$(COMMA)kernel/radio-ble \
	kernel/radio-wifi \
	watchdog-test-kernel \
	watchdog-test-idle

.PHONY: check-features
check-features: ## Clippy every non-default feature combination (Xtensa)
	@for f in $(FEATURE_CHECKS); do \
		echo "== demo --features $$f"; \
		$(CARGO) clippy --target $(XTENSA_TARGET) -Z build-std=core,compiler_builtins \
			-p demo --no-default-features \
			--features "kernel/$(BOARD),kernel/debug-level-1,$$f" \
			-- -D warnings || exit 1; \
	done

.PHONY: check-layers
check-layers: ## Enforce the three-layer dependency boundary (plan W7.1)
	$(BASH) tools/check-layers.sh

.PHONY: test-mutants
test-mutants: ## Break the kernel on purpose and check the race tests notice (needs a board)
	@$(PY) tools/mutate-selftests.py

.PHONY: device-matrix
device-matrix: ## Which drivers keep which device-class promises
	@$(BASH) tools/check-devices.sh

.PHONY: check-names
check-names: ## Enforce the package naming and layout convention
	$(BASH) tools/check-names.sh

.PHONY: test-host
test-host: test-boards ## Run host-side unit tests (every board manifest included)
	cargo test $(HOST_SELECT) --target $(HOST_TARGET) $(HOST_BOARD_FEATURES)

# Every board this tree can build for. A manifest's invariant tests only run
# for the board that is selected, so testing the default board alone leaves
# every other manifest unchecked -- which is how a pin or a panel layout stays
# wrong until someone flashes it.
BOARDS := board-esp32-wrover board-esp32-devkitc board-m5-atom-lite board-m5-atom-matrix board-m5-core2

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

ARM_PROBE_SERIAL   ?= 4150325537323116
ARM_BOOTSEL_SERIAL ?= E0C9125B0D9B

.PHONY: test-arm-target
test-arm-target: ## Build, flash, and judge ARM tests through Debug Probe
	$(MAKE) build APP=arm-selftest BOARD=board-wio-rp2040-mini
	pwsh -NoProfile -File tools/rp2040-run-selftest.ps1 \
		-ElfPath target/$(ARM_TARGET)/debug/arm-selftest \
		-ProbeSerial $(ARM_PROBE_SERIAL) -BootselSerial $(ARM_BOOTSEL_SERIAL)

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
# Applications are separate crates, so a pull never touches apps/*/<yours>/. What
# it does not do by itself is tell you what changed underneath them.
#
#   make upgrade            # pull, rebuild every app, report what broke
#   make upgrade PULL=0     # check against what is already checked out
.PHONY: upgrade
upgrade: ## Pull the latest FlintOS and report which applications it broke
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
	cargo clippy $(HOST_SELECT) --target $(HOST_TARGET) $(HOST_BOARD_FEATURES) \
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
	@printf 'FlintOS — Make targets:\n'
	@awk 'BEGIN { FS = ":.*## " } \
	     /^##@ / { printf "\n  %s\n", substr($$0, 5); next } \
	     /^[a-zA-Z0-9_-]+:.*## / { printf "    make %-16s %s\n", $$1, $$2 }' \
	     $(MAKEFILE_LIST)
	@printf '\n  Quick start:   make env  ->  make check  ->  make build\n'
	@printf '  Host tests:    make test-host    (no hardware; runs in CI)\n'
	@printf '  Board tests:   make test-target BOARD=board-m5-atom-matrix PORT=COM5\n\n'


