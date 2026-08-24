@echo off
rem SPDX-License-Identifier: Apache-2.0
rem Windows sibling of rustdoc-json.sh -- see that file for why.
set RUSTC_BOOTSTRAP=1
rustdoc %*
