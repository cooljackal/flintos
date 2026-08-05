<!-- SPDX-License-Identifier: Apache-2.0 -->

# Contributing to Flint

Thanks for your interest. Flint is early — the fastest way to help right now is
bring-up on real hardware and driver register audits against the ESP32 TRM.

## Developer Certificate of Origin

Flint requires a **DCO sign-off** on every commit. This is a lightweight
alternative to a CLA: you keep your copyright, and you assert that you have the
right to submit the code under Apache-2.0 — including the patent grant in
[section 3 of the licence](LICENSE).

Sign off by adding `-s` to your commit:

```bash
git commit -s -m "fix(uart): correct CONF0 bit-field placement"
```

That appends a trailer to your commit message:

```
Signed-off-by: Your Name <your.email@example.com>
```

The name and email must be real and must match your `git config user.name` and
`user.email`. By signing off you certify the
[Developer Certificate of Origin 1.1](https://developercertificate.org/).

Forgot to sign off? Fix the last commit with:

```bash
git commit --amend -s --no-edit
```

For a whole branch:

```bash
git rebase --signoff main
```

## Licensing new files

Every source file carries an SPDX identifier as its first line. Match the
comment syntax of the file type:

```rust
// SPDX-License-Identifier: Apache-2.0
```

```c
/* SPDX-License-Identifier: Apache-2.0 */
```

Shell scripts put it on the line *after* the shebang. Do not add per-file
copyright blocks — the SPDX line plus the root `LICENSE` and `NOTICE` are
sufficient.

## Before you open a pull request

```bash
make check        # host-side compile check
make test-host    # host unit tests
make lint         # clippy, warnings denied
make check-layers # three-layer boundary enforcement
```

`make check-all` additionally builds for Xtensa and needs the `esp` toolchain
(see the [README](README.md) for setup).

## The layer boundary is not negotiable

`tools/check-layers.sh` enforces the rule mechanically: **Layer-2 (`drivers/bus/`)
and Layer-3 (`drivers/logical/`) crates may depend only on `flint-api`.** A bus or
device driver that reaches for `flint-hal` or a `flint-arch-*` crate has gained
access to register definitions, which defeats the portability the three-layer
model exists to provide. CI fails on any violation.

If you genuinely need something from a lower layer, the fix is to widen the
`flint-api` surface — not to bypass the boundary.

## Hardware claims

Flint's history includes drivers that looked plausible but had wrong register
offsets, and asm that compiled cleanly but could not have run. So:

- Cite the ESP32 TRM section or the `esp-idf` header for any register offset,
  bit field, or interrupt-source number you add or change.
- If you have not run a change on real silicon, say so in the PR description.
  "Compiles" and "tests pass" are not evidence that hardware code works.
- Prefer a driver that returns an explicit error over one that returns a
  plausible-looking fake value.

## Commit messages

Conventional Commits, with the crate or subsystem as the scope:

```
fix(scheduler): preserve a2 across trap entry
feat(board): add M5Stack Atom board manifest
docs(readme): document the esp toolchain install
```

Explain *why* in the body, not just *what* — the diff already shows what.
