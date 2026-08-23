// SPDX-License-Identifier: Apache-2.0

fn main() {
    build::link();

    // The credentials are read from the environment at compile time with
    // `option_env!`. Cargo does not know the source depends on them, so tell it
    // to rebuild when they change — otherwise a second `make flash` with a
    // different network would silently reuse the first build.
    println!("cargo:rerun-if-env-changed=FLINT_WIFI_SSID");
    println!("cargo:rerun-if-env-changed=FLINT_WIFI_PASS");
}
