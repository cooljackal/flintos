#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Enable a driver in an app -- add it as a path dependency via cargo add.
#   make enable-driver APP=imu DRIVER=ssd1306
# DRIVER may be the package name (esp32-i2c) or the directory leaf (i2c).
set -eu

cd "$(dirname "$0")/.."
. tools/driver-catalog.sh

APP="${APP:-}"
DRIVER="${DRIVER:-}"
if [ -z "$APP" ] || [ -z "$DRIVER" ]; then
	echo "usage: make enable-driver APP=<app> DRIVER=<name>" >&2
	exit 1
fi

if [ ! -d "apps/examples/$APP" ] && [ ! -d "apps/tests/$APP" ]; then
	echo "error: no app named '$APP' under apps/examples/ or apps/tests/" >&2
	exit 1
fi

dir=$(resolve_driver "$DRIVER")
pkg=$(driver_pkg "$dir")

# An app may name only bus/logical drivers (check-layers). A physical driver is
# the board's to open; warn rather than silently create a layer violation.
if [ "$(driver_category "$dir")" = physical ]; then
	echo "warning: '$pkg' is a Layer-1 physical driver. An app may not depend on" >&2
	echo "         one directly (tools/check-layers.sh will reject it) -- a board" >&2
	echo "         opens the controller and hands the app a ready bus. Prefer a" >&2
	echo "         bus/logical driver, or add the device to the board manifest." >&2
fi

echo "Enabling $pkg (from $dir) in $APP ..."
cargo add "$pkg" --path "$dir" -p "$APP"
echo ""
echo "Done. '$pkg' is now a dependency of $APP; add 'use ${pkg//-/_};' in its source."
