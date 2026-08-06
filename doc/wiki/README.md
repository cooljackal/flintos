<!-- SPDX-License-Identifier: Apache-2.0 -->

# Wiki source

These files are the source of truth for the
[wiki](https://github.com/cooljackal/flintos/wiki). Edit them here, not on the
wiki — changes made on the wiki directly will be overwritten on the next sync.

Docs live in the repo so they can be reviewed in a PR alongside the change that
made them wrong, and so a checkout carries its own documentation.

## Publishing

```bash
tools/publish-wiki.sh
```

Clones the wiki repo, copies these files in, commits and pushes. Needs the wiki
to exist first — create any page once via the web UI, or GitHub has nothing to
clone.

## Adding a page

Drop a `.md` file here, add it to `_Sidebar.md`, and link it from `Home.md` if
it belongs in one of the front-page tables. Filenames become URLs: hyphens, no
spaces.

## House style

Short. Plain language. Get to the point.

- Lead with the command.
- A table beats a paragraph.
- Include context only where its absence would leave the reader guessing.
- Say the trap out loud. "GPIO12 pulled high at boot can brick the module" is
  worth more than three paragraphs on strapping-pin theory.
- No page should need reading to the end before anything works.
