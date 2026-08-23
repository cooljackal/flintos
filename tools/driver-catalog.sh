# SPDX-License-Identifier: Apache-2.0
#
# Shared helpers for the driver-catalog make targets (drivers, enable-driver,
# disable-driver). Sourced, not run. A "driver" is a crate under
# drivers/{physical/<soc>,bus,logical}/<leaf>; the `_template` scaffold is
# skipped everywhere.

# Every driver crate directory, one per line (no trailing slash).
#
# Read from the workspace `members` list rather than a `drivers/*/*/` glob: the
# members list is where a driver is actually registered (add-driver writes it),
# and a shell glob inside a function proved unreliable under a make recipe's
# non-interactive bash -- it came back empty and the catalog silently vanished.
# Reading one file at the (caller-established) repo root has no such dependency.
list_driver_dirs() {
	sed -n '/^[[:space:]]*members = \[/,/^\]/p' Cargo.toml |
		sed -n 's/^[[:space:]]*"\(drivers\/[^"]*\)".*/\1/p' |
		while IFS= read -r d; do
			case "$(basename "$d")" in _*) continue ;; esac
			[ -f "$d/Cargo.toml" ] && printf '%s\n' "$d"
		done
	# A while-read loop exits non-zero at EOF; without this the function's
	# failure status aborts `$(list_driver_dirs)` under a caller's `set -e`.
	return 0
}

driver_category() {
	case "$1" in
	drivers/physical/*) echo physical ;;
	drivers/bus/*) echo bus ;;
	drivers/logical/*) echo logical ;;
	*) echo "?" ;;
	esac
}

# awk (exit on first match) rather than `sed ... | head -1`: the pipe form races
# on SIGPIPE and can intermittently yield an empty string, which would misresolve.
driver_pkg() { awk -F'"' '/^name = "/ {print $2; exit}' "$1/Cargo.toml"; }
driver_desc() { awk -F'"' '/^description = "/ {print $2; exit}' "$1/Cargo.toml"; }

# resolve_driver <query> -> prints the matching driver dir, or fails with a
# message. <query> matches either the package name (esp32-i2c) or the directory
# leaf (i2c).
resolve_driver() {
	q="$1"
	# Capture the catalog once into a variable. A double-nested command
	# substitution -- `$(for d in $(list_driver_dirs) ...)` -- came back empty
	# under a make recipe's bash; a single level does not.
	all=$(list_driver_dirs)
	matches=""
	for d in $all; do
		if [ "$(driver_pkg "$d")" = "$q" ] || [ "$(basename "$d")" = "$q" ]; then
			matches="${matches}${d}
"
		fi
	done
	matches=$(printf '%s' "$matches" | grep . | sort -u || true)
	if [ -z "$matches" ]; then
		echo "error: no driver matches '$q'. Run 'make drivers' to see the catalog." >&2
		return 1
	fi
	# `matches` has had its trailing newline stripped by command substitution, so
	# count with grep -c (a lone entry with no newline would make `wc -l` say 0).
	if [ "$(printf '%s\n' "$matches" | grep -c .)" != 1 ]; then
		echo "error: '$q' is ambiguous:" >&2
		printf '%s\n' "$matches" | sed 's/^/  /' >&2
		echo "Use the full package name." >&2
		return 1
	fi
	printf '%s\n' "$matches"
}
