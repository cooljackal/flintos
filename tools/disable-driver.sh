#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Disable a driver in an app -- removes the path dependency via `cargo remove`.
#   make disable-driver APP=imu DRIVER=ssd1306
# DRIVER may be the package name (esp32-i2c) or the directory leaf (i2c).
set -eu

cd "$(dirname "$0")/.."
. tools/driver-catalog.sh

APP="${APP:-}"
DRIVER="${DRIVER:-}"
if [ -z "$APP" ] || [ -z "$DRIVER" ]; then
	echo "usage: make disable-driver APP=<app> DRIVER=<name>" >&2
	exit 1
fi

if [ ! -d "apps/examples/$APP" ] && [ ! -d "apps/tests/$APP" ]; then
	echo "error: no app named '$APP' under apps/examples/ or apps/tests/" >&2
	exit 1
fi

# Resolve to a package name when possible, but fall back to the query itself so
# a driver that no longer exists on disk can still be removed from the manifest.
if dir=$(resolve_driver "$DRIVER" 2>/dev/null); then
	pkg=$(driver_pkg "$dir")
else
	pkg="$DRIVER"
fi

echo "Disabling $pkg in $APP ..."
cargo remove "$pkg" -p "$APP"
echo "Done."
