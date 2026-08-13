<!-- SPDX-License-Identifier: Apache-2.0 -->

# Working on FlintOS

Instructions for AI agents. `CLAUDE.md` points here.

## Answer format

**Start with a bottom line. Then a table or bullets. Nothing else.**

- **Every reply opens with the bottom line** — one or two sentences saying
  what happened and what it means. Not a heading, not a preamble, not a recap
  of the question. If the reply is one line long, that line is the bottom
  line.
- **Tables and bullets, not prose.** A table for results — what was done,
  what came of it. Bullets where a table does not fit. A finding is one line.
- **Plain language.** Say what a thing *is* before naming it, and prefer the
  plain word to the symbol: "the receiver's interrupt mask was never switched
  on" rather than `WMAC_INT_ENA == 0`. Register names, symbol names, file
  paths and line numbers are evidence — put them in the table cell or the
  commit message, not in the sentence carrying the meaning. A reply should be
  readable by someone who has not been staring at this code all week.
- **Short.** The user asks for detail when they want it, and asking is
  cheaper than reading past it. Past ~15 lines, cut.
- No section headings unless there are genuinely three or more sections.
- Do not narrate the work, re-explain a fix already described in a commit
  message, or restate the same point in a summary at the end.
- Detail belongs in the commit message and the code comments, which is where
  someone will look for it later. The chat reply is a status report.
- **Say which parts are measured and which are inferred.** A plausible
  mechanism is not a finding. This project has repeatedly had a confident
  explanation killed by the next measurement.

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
