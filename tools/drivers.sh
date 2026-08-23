#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# The driver catalog: what a Layer-2/3 app can depend on. Filter with
#   make drivers                       # everything
#   make drivers CATEGORY=logical      # one tier
#   make drivers MATCH=i2c             # name/description substring (case-insensitive)
#   make drivers CATEGORY=physical MATCH=spi
set -eu

cd "$(dirname "$0")/.."
. tools/driver-catalog.sh

CATEGORY="${CATEGORY:-}"
MATCH="${MATCH:-}"

case "$CATEGORY" in
"" | physical | bus | logical) ;;
*)
	echo "error: CATEGORY must be physical, bus, or logical (got '$CATEGORY')" >&2
	exit 1
	;;
esac

# Build the whole catalog in ONE awk pass over every driver's Cargo.toml, as
# "category<TAB>package<TAB>description" lines. Per-driver `basename`/`awk`/
# subshells were forking hundreds of processes -- slow, especially on Windows.
dirs=$(list_driver_dirs)
catalog=$(
	# shellcheck disable=SC2086  # word-splitting the dir list is intended
	printf '%s/Cargo.toml\n' $dirs | tr '\n' '\0' | xargs -0 awk -F'"' '
		function emit() { if (path != "") print cat "\t" name "\t" desc }
		FNR == 1 {
			emit(); name = ""; desc = ""; path = FILENAME
			cat = (path ~ /drivers\/physical\//) ? "physical" \
			    : (path ~ /drivers\/bus\//)      ? "bus" \
			    :                                  "logical"
		}
		/^name = "/        && name == "" { name = $2 }
		/^description = "/ && desc == "" { desc = $2 }
		END { emit() }
	'
)

# One flat, aligned row per driver -- CATEGORY first so `make drivers | grep
# physical` (or by name/description) just works. The banner, filter note and
# column header go to stderr, so a pipe on stdout sees only data rows.
echo "Driver catalog -- enable in an app with: make enable-driver APP=<app> DRIVER=<name>" >&2
[ -n "$CATEGORY$MATCH" ] && echo "filter: ${CATEGORY:+category=$CATEGORY }${MATCH:+match=$MATCH}" >&2
printf '%-9s %-16s %s\n' "CATEGORY" "PACKAGE" "DESCRIPTION" >&2

shown=0
# physical, bus, logical order; within a tier, the workspace members order.
# No `sort`: under a make recipe on Windows, `sort` resolves to System32
# sort.exe, which rejects -t/-k ("Input file specified two times").
for cat in physical bus logical; do
	[ -n "$CATEGORY" ] && [ "$CATEGORY" != "$cat" ] && continue
	while IFS="	" read -r c pkg desc; do
		[ "$c" = "$cat" ] || continue
		if [ -n "$MATCH" ]; then
			printf '%s\n' "$c $pkg $desc" | grep -iqF -- "$MATCH" || continue
		fi
		printf '%-9s %-16s %s\n' "$c" "$pkg" "$desc"
		shown=$((shown + 1))
	done <<EOF
$catalog
EOF
done

[ "$shown" = 0 ] && echo "(no drivers match)" >&2
exit 0
