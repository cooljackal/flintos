#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# RUSTDOC wrapper for `make apidoc`: give *rustdoc* the nightly powers the JSON
# output format needs (RUSTC_BOOTSTRAP=1) while leaving *cargo* on stable, so
# cargo does not activate the `[unstable] build-std` in .cargo/config.toml --
# which on the host target drags in a second `core` and breaks rustdoc with a
# duplicate-lang-item error. cargo runs stable => sysroot core only => clean.
export RUSTC_BOOTSTRAP=1
exec rustdoc "$@"
