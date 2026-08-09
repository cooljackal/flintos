# Upgrading

```bash
make upgrade
```

Pulls, rebuilds every application in `apps/`, and tells you which ones broke
and why.

FlintOS moves fast. Your application is a separate crate, so a pull never
touches `apps/<yours>/` — what it does change is the kernel underneath it.

## What you get

```
Updated 990ff28 -> a1b2c3d

The application-facing API changed:
  api/src/task.rs

CHANGELOG.md changed. Breaking entries now present:
  - Applications must declare an ABI version. ...

==> Rebuilding 2 application(s)
  demo           ok
  hello          BROKEN

1 application(s) broke.

--- hello ---
  error: FlintOS ABI mismatch: this application declares `abi = 1`, ...
  Apply the Breaking entries above, then bump the abi in flint_app!.
```

Exits non-zero if anything broke, so it works in a script.

| Option | Effect |
|---|---|
| `make upgrade PULL=0` | Check what's already checked out — no pull |
| `make upgrade BOARD=board-m5-atom-matrix` | Rebuild against a different board |

It refuses to pull with uncommitted changes. An upgrade merged into dirty state
is hard to unpick when it goes wrong.

## The ABI declaration

Every application states what it was written against:

```rust
kernel::flint_app!(main, abi = 1);
```

The kernel checks it at compile time. A mismatch fails the build naming the
cause, instead of erroring somewhere in your own code:

```
error: FlintOS ABI mismatch: this application declares `abi = 1`, which is not
       the ABI this kernel provides (see `api::ABI`).
       Read the Breaking entries in CHANGELOG.md, apply them, then update the
       declaration in flint_app!.
  --> apps/hello/src/main.rs:17:1
```

The number bumps whenever the surface you compile against changes
incompatibly — a signature in `api`, a `hal` type you name, the `flint_app!`
contract, or a board manifest's shape.

## What to do when something breaks

1. Read the **Breaking** entries in
   [CHANGELOG.md](https://github.com/cooljackal/flintos/blob/main/CHANGELOG.md).
   Each says what to change, not just what changed.
2. Apply them.
3. Bump the `abi` number in your `flint_app!`.
4. `make upgrade PULL=0` to confirm.

Step 3 is deliberately manual. Bumping it automatically would let an
application claim compatibility nobody checked.

## Pinning instead

If you'd rather upgrade on your own schedule, keep your application in its own
repository and depend on FlintOS by git rev:

```toml
kernel = { git = "https://github.com/cooljackal/flintos", rev = "990ff28", package = "kernel" }
```

Upgrading becomes changing the rev — explicit and reviewable. This isn't fully
supported yet; see
[#45](https://github.com/cooljackal/flintos/issues/45).

## For contributors

CI fails a pull request that touches `api/`, `hal/` or `board/` without
updating `CHANGELOG.md`. Those crates are what applications compile against, so
a change there can break code in a repository nobody on the PR can see, and
this file is the only way its author finds out what to do.

If the change genuinely cannot affect an application, put it under **Added** or
**Fixed** rather than skipping the file.
