<!-- SPDX-License-Identifier: Apache-2.0 -->

# Working on FlintOS

Instructions for AI agents. `CLAUDE.md` points here.

## Answer format

**Bottom line, key findings, next steps. Nothing else.**

- Lead with the answer. No preamble, no recap of what was asked.
- Bullets, not prose. A finding is one line plus its cost.
- No section headings unless there are genuinely three or more sections.
- Do not narrate the work, re-explain a fix already described in a commit
  message, or restate the same point in a summary at the end.
- Detail belongs in the commit message and the code comments, which is where
  someone will look for it later. The chat reply is a status report.
- Long output is a failure mode, not thoroughness. If a reply runs past ~15
  lines, cut it.

## Rules

- **The user pushes. Never `git push`.** Commit locally and say so.
- **`git commit -s`** — every commit needs the DCO sign-off.
- **Stage by explicit path.** Never `git add -A` or `git add .`: another
  session may share this working tree, and a blanket add has swept its work
  into an unrelated commit before.
- **`SPDX-License-Identifier: Apache-2.0`** at the top of every source file.
- **Nothing untested reaches `main`.** "It compiles" is not tested. Say
  plainly what was run and what was not.

## Before every commit

```bash
make test-host && make lint && make check-layers && make check-all
```

`make build APP=<app>` for anything target-only. On-target changes that no
host test covers need a board, and if one is not attached, say so rather than
implying coverage.

## Two lessons this project paid for

**Read the vendor source.** esp-idf, Zephyr, NuttX and Arduino are all
available and all disagree with guesswork. The flash driver was broken for a
week because of reasoning from symptoms; reading `spi_flash_rom_patch.c`
answered it in one sitting.

**This tree's own comments are not evidence.** Several have been confidently
wrong, and at least one caused a bug. Verify against the code before relying
on a comment, and fix it when it lies — a wrong comment is worse than none.
